//! Type inference context and constraint generation.
//!
//! The inference context tracks type information during type checking:
//! expression types, type variable generation, and constraint collection.
//! Constraints are solved later via unification.
//!
//! # Testing Philosophy
//!
//! This module has no unit tests. The type checker requires the full pipeline
//! (source -> lex -> parse -> CST -> AST -> resolve -> typecheck) to function,
//! making isolated unit tests impractical and misleading. Previous unit tests
//! that constructed synthetic AST nodes:
//!
//! 1. Failed to catch real bugs (which were only caught by integration tests)
//! 2. Tested implementation details (e.g., "should have 2 Eq constraints")
//! 3. Required fragile manual AST construction
//! 4. Provided false confidence in test coverage
//!
//! All type checker behavior is tested through the integration test scripts in
//! `scripts/*.rumps`, which exercise the complete pipeline and use snapshot
//! testing for regression detection. This approach has proven far more effective
//! at catching bugs in practice.

mod constraint_region;
mod convert;
mod expr;
mod hoist;
mod pattern;
mod scheme;
mod stmt;

use std::collections::{HashMap, HashSet};
use std::mem;

use indexmap::IndexMap;
use nonempty::NonEmpty;
use smallvec::SmallVec;

use self::constraint_region::ConstraintRegion;
use super::convert::is_in_module;
use super::decl::TypeDeclRegistry;
use super::env::{MethodRefOrigin, TypeEnv};
use super::error::{TyPrinter, TypeError};
use super::instance::{Instance, InstanceRegistry};
use super::ty::{Rename, Scheme, Ty, TyArena, TyId, TyVar, TypeClass};
use super::uf::UnionFind;
use super::unify::{NewtypeEdge, NewtypeEdgeStatus, SolveCtx};
use super::{
    CheckedExprAux, CheckedExprInfo, CheckedNewtypeEdgeRuntimeInfo,
    CheckedTypePatternInfo, TypecheckOutput,
};
use crate::ast::{
    self, AssocTypeDef, AstClassConstraints, AstTypeExprId, ExprId,
    InstanceMethodDef, MatchPatternId, Stmt, StmtId, TxnId, TypeParam,
};
use crate::env::Environment;
use crate::error::Result;
use crate::intern::{self, QualifiedName, StringId, StringInterner};
use crate::value::{self, TypeId, TypeRegistry};
use crate::{ClassId, Error, Span};

impl UnionFind {
    /// Resolve all `TyId` values in a map through this union-find.
    fn resolve_map<K>(
        &mut self,
        map: &mut HashMap<K, TyId>,
        arena: &mut TyArena,
    ) {
        map.values_mut()
            .for_each(|ty| *ty = self.resolve(*ty, arena));
    }
}

/// Fields populated during inference that are passed directly to the
/// interpreter. These are write-only during inference (no post-processing)
/// and moved into `TypecheckOutput` at the end.
pub(super) struct InterpreterOutput {
    /// Cache of compiled regex patterns (validated during typechecking).
    ///
    /// Regex literals are compiled here; invalid patterns produce type errors.
    /// The interpreter retrieves compiled patterns by index.
    pub(super) regex_cache: Vec<regex::Regex>,
    /// Per-expression runtime metadata overrides.
    pub(super) expr_metadata: HashMap<ExprId, CheckedExprInfo>,
    /// Expressions widened into a union with their concrete member type.
    pub(super) union_value_reprs: HashMap<ExprId, TyId>,
    /// Function and closure types keyed by body expression.
    pub(super) function_types: HashMap<ExprId, TyId>,
    /// Checked target types for `as`, `read`, and annotation expressions.
    pub(super) expr_targets: HashMap<ExprId, TyId>,
    /// Newtype representation edges approved for runtime execution.
    pub(super) approved_newtype_edges:
        HashMap<ExprId, CheckedNewtypeEdgeRuntimeInfo>,
    /// Checked type facts for `expr is Pattern` expression patterns.
    pub(super) is_patterns: HashMap<ExprId, CheckedTypePatternInfo>,
    /// Checked type annotation targets for `let` bindings, keyed by RHS expr.
    pub(super) let_targets: HashMap<ExprId, TyId>,
    /// Checked target types for `name IS Type` match patterns.
    pub(super) match_targets: HashMap<MatchPatternId, TyId>,
    /// Maps solved alias `TyId`s to their expanded underlying `TyId`s.
    ///
    /// Populated for `read` targets so runtime object-field reads can resolve
    /// nested aliases without reinterpreting AST type expressions.
    pub(super) alias_type_expansions: HashMap<TyId, TyId>,
}

impl InterpreterOutput {
    fn new() -> Self {
        Self {
            regex_cache: Vec::new(),
            expr_metadata: HashMap::new(),
            union_value_reprs: HashMap::new(),
            function_types: HashMap::new(),
            expr_targets: HashMap::new(),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::new(),
            let_targets: HashMap::new(),
            match_targets: HashMap::new(),
            alias_type_expansions: HashMap::new(),
        }
    }

    /// Resolve all type-variable-bearing maps through the union-find.
    fn resolve(&mut self, uf: &mut UnionFind, arena: &mut TyArena) {
        self.expr_metadata
            .values_mut()
            .for_each(|info| info.resolve(uf, arena));
        uf.resolve_map(&mut self.union_value_reprs, arena);
        uf.resolve_map(&mut self.function_types, arena);
        uf.resolve_map(&mut self.expr_targets, arena);
        self.approved_newtype_edges
            .values_mut()
            .for_each(|info| info.resolve(uf, arena));
        self.is_patterns
            .values_mut()
            .for_each(|info| info.resolve(uf, arena));
        uf.resolve_map(&mut self.let_targets, arena);
        uf.resolve_map(&mut self.match_targets, arena);
        self.alias_type_expansions = mem::take(&mut self.alias_type_expansions)
            .into_iter()
            .map(|(alias, expanded)| {
                (uf.resolve(alias, arena), uf.resolve(expanded, arena))
            })
            .collect();
    }

    pub(super) fn set_expr_ty(&mut self, id: ExprId, ty: TyId) {
        self.expr_metadata.entry(id).or_default().ty = Some(ty);
    }

    pub(super) fn set_concrete_expr_ty(&mut self, id: ExprId, ty: TyId) {
        let info = self.expr_metadata.entry(id).or_default();
        info.ty = Some(ty);
        info.concrete = true;
    }

    pub(super) fn set_regex_index(&mut self, id: ExprId, idx: u32) {
        let info = self.expr_metadata.entry(id).or_default();
        info.ty = Some(TyArena::REGEX);
        info.aux = CheckedExprAux::RegexIndex(idx);
    }

    pub(super) fn set_hof_call(&mut self, id: ExprId, out: TyId) {
        let info = self.expr_metadata.entry(id).or_default();
        info.ty = Some(out);
        info.aux = match info.aux {
            CheckedExprAux::InstanceCall { .. } => info.aux,
            CheckedExprAux::HofCall { class, .. } => {
                CheckedExprAux::HofCall { out, class }
            }
            CheckedExprAux::NakedMethod { class } => CheckedExprAux::HofCall {
                out,
                class: Some(class),
            },
            _ => CheckedExprAux::HofCall { out, class: None },
        };
    }

    pub(super) fn set_instance_call(&mut self, id: ExprId, recv: TyId) {
        let info = self.expr_metadata.entry(id).or_default();
        info.aux = match info.aux {
            CheckedExprAux::InstanceCall { fun, class, .. } => {
                CheckedExprAux::InstanceCall {
                    recv: Some(recv),
                    fun,
                    class,
                }
            }
            CheckedExprAux::NakedMethod { class } => {
                CheckedExprAux::InstanceCall {
                    recv: Some(recv),
                    fun: None,
                    class: Some(class),
                }
            }
            CheckedExprAux::HofCall { class, .. } => {
                CheckedExprAux::InstanceCall {
                    recv: Some(recv),
                    fun: None,
                    class,
                }
            }
            _ => CheckedExprAux::InstanceCall {
                recv: Some(recv),
                fun: None,
                class: None,
            },
        };
    }

    pub(super) fn set_instance_fun(&mut self, id: ExprId, fun: StringId) {
        let info = self.expr_metadata.entry(id).or_default();
        info.aux = match info.aux {
            CheckedExprAux::InstanceCall { recv, class, .. } => {
                CheckedExprAux::InstanceCall {
                    recv,
                    fun: Some(fun),
                    class,
                }
            }
            CheckedExprAux::NakedMethod { class } => {
                CheckedExprAux::InstanceCall {
                    recv: None,
                    fun: Some(fun),
                    class: Some(class),
                }
            }
            CheckedExprAux::HofCall { class, .. } => {
                CheckedExprAux::InstanceCall {
                    recv: None,
                    fun: Some(fun),
                    class,
                }
            }
            _ => CheckedExprAux::InstanceCall {
                recv: None,
                fun: Some(fun),
                class: None,
            },
        };
    }

    pub(super) fn set_naked_method(&mut self, id: ExprId, class: StringId) {
        let info = self.expr_metadata.entry(id).or_default();
        info.aux = match info.aux {
            CheckedExprAux::InstanceCall { recv, fun, .. } => {
                CheckedExprAux::InstanceCall {
                    recv,
                    fun,
                    class: Some(class),
                }
            }
            CheckedExprAux::HofCall { out, .. } => CheckedExprAux::HofCall {
                out,
                class: Some(class),
            },
            _ => CheckedExprAux::NakedMethod { class },
        };
    }
}

/// Input for `hoist_class_instance` and `class_instance`.
///
/// The type parameter `A` distinguishes hoisting (where associated types are
/// not yet processed) from full type checking (where they are).
pub(super) struct ClassInstanceInput<'a, A = ()> {
    pub(super) class_name: StringId,
    pub(super) class_args: &'a SmallVec<[AstTypeExprId; 2]>,
    pub(super) type_params: &'a SmallVec<[TypeParam; 2]>,
    pub(super) for_type: AstTypeExprId,
    pub(super) constraints: &'a SmallVec<[(StringId, AstClassConstraints); 2]>,
    pub(super) methods: &'a SmallVec<[InstanceMethodDef; 4]>,
    pub(super) assoc_types: A,
    pub(super) module: Option<QualifiedName>,
    pub(super) span: Span,
}

/// Input for `instance_method`.
pub(super) struct InstanceMethodInput<'a> {
    pub(super) class: ClassId,
    pub(super) for_ty: TyId,
    pub(super) class_arg_tys: &'a SmallVec<[TyId; 2]>,
    pub(super) type_param_subst: &'a IndexMap<StringId, TyId>,
    pub(super) method: &'a InstanceMethodDef,
    pub(super) inst_span: Span,
}

/// State for hoisting and forward-reference tracking.
///
/// Populated during Pass 1 (hoisting) and consumed/updated during Pass 2
/// (finalization). Logically cohesive; methods on this struct take a
/// `&mut HoistCtx` for access to shared `InferCtx` state.
pub(super) struct HoistState {
    /// Finalized simple `let` bindings, keyed by statement.
    ///
    /// Pass `2` rebinds these into the active lexical scope without
    /// re-inferring the RHS, so later same-name module `let`s do not leak into
    /// earlier modules or top-level scopes.
    pub(super) final_let_schemes:
        HashMap<StmtId, (StringId, Scheme, Option<MethodRefOrigin>)>,
    /// Hoisted polymorphic schemes that have not yet been finalized by Pass 2.
    ///
    /// Populated by `hoist_fun` (top level, modules, blocks). The key is
    /// the `StmtId` where the hoist originated; the value is the Pass 1
    /// scheme so we can recognize forward-ref instantiations as referring
    /// to it.
    ///
    /// An entry is removed when Pass 2 calls `finalize_hoisted_fun` for
    /// that `StmtId`. Only entries that are still present at lookup time
    /// count as "not finalized"; once removed, lookups go straight to the
    /// env binding (which holds the Pass 2 final scheme).
    pub(super) funs: HashMap<StmtId, Scheme>,
    /// Reverse index from hoisted scheme `TyId` to `StmtId`.
    ///
    /// Keyed by the `Scheme::ty` field of each entry in `funs`.
    /// Since each `hoist_fun` call allocates a fresh function `TyId`, these
    /// are unique per hoisted scheme.
    pub(super) fun_index: HashMap<TyId, StmtId>,
    /// Forward-reference instantiations of hoisted polymorphic schemes.
    ///
    /// When `var` (or `Expr::Path`) looks up a name and the returned scheme
    /// is the Pass 1 hoisted scheme of a function whose Pass 2 finalization
    /// has not yet run, the call site records the freshly-instantiated
    /// function type here.
    ///
    /// `finalize_hoisted_fun` drains the entry, instantiates the final
    /// Pass 2 scheme afresh once per recorded instantiation, and unifies.
    pub(super) forward_instantiations:
        HashMap<StmtId, Vec<(TyId, Span, Option<QualifiedName>)>>,
    /// Functions whose Pass 2 finalization has completed.
    ///
    /// Each entry records the function's name, its quantified type
    /// variables, declared type parameters, and its module context (if any).
    /// Used by `finalize_hoisted_fun` to detect when a forward-ref replay
    /// emits constraints that (through union-find) apply to a
    /// previously-finalized function's quantified vars, enabling
    /// retroactive scheme enrichment.
    finalized_funs: Vec<FinalizedFun>,
    /// Var mappings from replay instantiations (scheme var -> fresh var).
    ///
    /// Used by `enrich_finalized_schemes` to add virtual edges that bridge
    /// quantified vars with their replay instantiation vars, enabling
    /// transitive constraint propagation through arbitrary cycle lengths.
    ///
    /// NOTE: grows monotonically during hoisted function finalization and
    /// is cloned on each `enrich_finalized_schemes` call. Both this and
    /// `finalized_funs` are cleared once all hoisted functions have been
    /// finalized (`funs` is empty). In pathological cases with
    /// many separate groups of mutually recursive hoisted functions,
    /// the per-enrichment clone cost is O(total replay maps so far);
    /// this is acceptable because such programs are extremely rare.
    replay_var_maps: Vec<SmallVec<[(TyVar, TyVar); 4]>>,
}

#[derive(Default)]
pub(super) struct LetTvFrame {
    names: HashMap<TyVar, StringId>,
    cs: Vec<(TyVar, TypeClass<TyId>)>,
}

impl HoistState {
    fn new() -> Self {
        Self {
            final_let_schemes: HashMap::new(),
            funs: HashMap::new(),
            fun_index: HashMap::new(),
            forward_instantiations: HashMap::new(),
            finalized_funs: Vec::new(),
            replay_var_maps: Vec::new(),
        }
    }

    /// Record a forward-ref instantiation if `scheme` matches a
    /// not-yet-finalized hoisted scheme.
    ///
    /// Uses `fun_index` for O(1) lookup by the scheme's function
    /// `TyId`, then confirms identity via structural `Scheme` equality.
    /// `inst_ty` is the freshly-instantiated function type from
    /// `scheme.instantiate`; `span` is the call site span.
    pub(super) fn record_forward_ref(
        &mut self,
        scheme: &Scheme,
        inst_ty: TyId,
        span: Span,
        module: Option<QualifiedName>,
    ) {
        if let Some(&stmt_id) = self.fun_index.get(&scheme.ty) {
            let is_match = self.funs.get(&stmt_id).is_some_and(|s| s == scheme);
            if is_match {
                self.forward_instantiations
                    .entry(stmt_id)
                    .or_default()
                    .push((inst_ty, span, module));
            }
        }
    }

    /// Finalize a hoisted function: drain forward-ref instantiations
    /// recorded against its Pass 1 scheme and retroactively unify each
    /// one with a fresh instantiation of the Pass 2 final scheme.
    ///
    /// Call this from `fun()` (and any other Pass 2 site that finalizes
    /// a hoisted function) immediately AFTER binding the final scheme
    /// into the environment.
    ///
    /// After replay, uses a union-find snapshot to temporarily establish
    /// connectivity from all deferred `Unify`/`Callable` constraints,
    /// then checks whether any newly-emitted class constraints reach
    /// a previously-finalized function's quantified vars. If so,
    /// enriches that function's scheme so future instantiations carry
    /// the constraint. The snapshot is rolled back afterwards.
    pub(super) fn finalize_hoisted_fun(
        &mut self,
        cx: &mut HoistCtx<'_>,
        stmt_id: StmtId,
        name: StringId,
        declared_tvs: HashSet<TyVar>,
        tv_names: HashMap<TyVar, StringId>,
    ) {
        // Drop the Pass 1 scheme and its reverse index entry.
        // Retain the constraint count so we can skip already-emitted
        // constraints during replay (the Pass 1 scheme's constraints
        // were emitted at the original forward-ref instantiation site;
        // only body-derived constraints added by Phase 3 are new).
        let pass1_n = self
            .funs
            .remove(&stmt_id)
            .map(|scheme| {
                self.fun_index.remove(&scheme.ty);
                scheme.constraints.len()
            })
            .unwrap_or(0);

        // Register this function's quantified vars (and module context)
        // for retroactive enrichment by later finalizations.
        if let Some(scheme) = cx.env.lookup(name).cloned() {
            if !scheme.vars.is_empty() {
                self.finalized_funs.push(FinalizedFun {
                    name,
                    vars: scheme.vars.clone(),
                    declared_tvs,
                    tv_names,
                    mod_ctx: cx.current_module.clone(),
                });
            }
        }

        // Replay each recorded forward-ref instantiation against
        // the Pass 2 final scheme.
        if let Some(insts) = self.forward_instantiations.remove(&stmt_id) {
            let final_scheme = cx.env.lookup(name).cloned();
            if let Some(final_scheme) = final_scheme {
                let constraint_start = cx.constraints.len();

                insts.into_iter().for_each(|(forward_ty, span, module)| {
                    let (ty_inst, constraints, var_map) =
                        final_scheme.instantiate_tracked(cx.uf, cx.ty_arena);
                    // Only emit constraints beyond those already emitted
                    // by the original Pass 1 instantiation.
                    constraints.into_iter().skip(pass1_n).for_each(
                        |(ty, class)| {
                            cx.constraints.push((
                                Constraint::Class { ty, class, span },
                                module.clone(),
                            ));
                        },
                    );
                    if !var_map.is_empty() {
                        self.replay_var_maps.push(var_map);
                    }
                    cx.constraints.push((
                        Constraint::Unify(forward_ty, ty_inst, span),
                        module,
                    ));
                });

                // Check whether the replay introduced class constraints
                // that (through deferred Unify/Callable chains) reach a
                // previously-finalized function's quantified vars. Use
                // snapshot/rollback to temporarily union all deferred
                // constraints, check, then rollback.
                let has_new_class = cx.constraints[constraint_start..]
                    .iter()
                    .any(|(c, _)| matches!(c, Constraint::Class { .. }));

                if has_new_class && !self.finalized_funs.is_empty() {
                    self.enrich_finalized_schemes(cx, constraint_start);
                }
            } else {
                invariant!("scheme bound by Pass 2 fun")
            }
        }

        // Once all hoisted functions have been finalized, the
        // enrichment bookkeeping is no longer needed; clear to
        // avoid accumulating stale data.
        if self.funs.is_empty() {
            self.finalized_funs.clear();
            self.replay_var_maps.clear();
        }
    }

    /// Enrich previously-finalized schemes with class constraints
    /// discovered during a finalization replay.
    ///
    /// Temporarily establishes union-find connectivity from ALL
    /// deferred `Unify`/`Callable` constraints (snapshot + rollback),
    /// plus virtual edges from `replay_var_maps` that bridge quantified
    /// vars with their replay instantiation vars (enabling transitive
    /// constraint propagation through cycles of any length).
    ///
    /// Also updates the module member registry when an enriched
    /// function lives inside a module.
    fn enrich_finalized_schemes(
        &mut self,
        cx: &mut HoistCtx<'_>,
        constraint_start: usize,
    ) {
        // Build root-to-origvar map for finalized functions.
        // Keyed on `(StringId, Option<QualifiedName>)` to disambiguate
        // functions with the same name in different module contexts.
        type FunKey = (StringId, Option<QualifiedName>);

        let end = cx.constraints.len();
        let fin = self.finalized_funs.clone();
        let var_maps = self.replay_var_maps.clone();

        let snap = cx.uf.snapshot();

        ConstraintRegion::build_unions(
            cx.constraints,
            0..end,
            cx.uf,
            cx.ty_arena,
        );

        // Virtual edges: union each scheme var with its replay
        // instantiation vars to bridge the gap for cycles >= 3.
        var_maps.iter().for_each(|vm| {
            vm.iter().for_each(|&(scheme_v, replay_v)| {
                let rs = cx.uf.find(scheme_v);
                let rr = cx.uf.find(replay_v);
                cx.uf.union(rs, rr);
            });
        });

        let mut root_to_orig: HashMap<TyVar, Vec<(FunKey, TyVar)>> =
            HashMap::new();

        fin.iter().for_each(|f| {
            let key = (f.name, f.mod_ctx.clone());
            f.vars.iter().for_each(|&fv| {
                root_to_orig
                    .entry(cx.uf.find(fv))
                    .or_default()
                    .push((key.clone(), fv));
            });
        });

        // Collect scheme updates from new class constraints.
        // Temporarily take constraints to scan while holding `&mut cx`
        // for UF lookups.
        let cs = mem::take(cx.constraints);
        let mut updates: HashMap<
            FunKey,
            SmallVec<[(TyVar, TypeClass<TyId>, Span); 2]>,
        > = HashMap::new();
        cs[constraint_start..end].iter().for_each(|(c, _)| {
            if let Constraint::Class { ty, class, span } = c {
                if let Ty::Var(cv) = cx.ty_arena.get(*ty) {
                    let root = cx.uf.find(*cv);
                    if let Some(entries) = root_to_orig.get(&root) {
                        entries.iter().for_each(|(fkey, fv)| {
                            updates.entry(fkey.clone()).or_default().push((
                                *fv,
                                class.clone(),
                                *span,
                            ));
                        });
                    }
                }
            }
        });

        *cx.constraints = cs;

        cx.uf.rollback(snap);

        // Build a lookup from `FunKey` to `&FinalizedFun` for
        // checking `declared_tvs` during update application.
        let fin_lookup: HashMap<FunKey, &FinalizedFun> = fin
            .iter()
            .map(|f| ((f.name, f.mod_ctx.clone()), f))
            .collect();

        // Apply updates to env bindings and module member registry.
        updates.into_iter().for_each(|(key, new_cs)| {
            if let Some(mut scheme) = cx.env.lookup(key.0).cloned() {
                let fin_entry = fin_lookup.get(&key);
                let mut changed = false;
                let mut rejected: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
                    SmallVec::new();

                new_cs.into_iter().for_each(|(fv, class, span)| {
                    let entry = (fv, class.clone());
                    if !scheme.constraints.contains(&entry)
                        && !rejected.contains(&entry)
                    {
                        if fin_entry
                            .is_some_and(|f| f.declared_tvs.contains(&fv))
                        {
                            let param = fin_entry
                                .and_then(|f| f.tv_names.get(&fv))
                                .map(|&n| cx.env.resolve_string(n))
                                .unwrap_or_else(|| "?".to_owned());
                            cx.errors.push(
                                TypeError::MissingTypeParamConstraint {
                                    param,
                                    class,
                                    span,
                                },
                            );
                            rejected.push(entry);
                        } else {
                            scheme.constraints.push(entry);
                            changed = true;
                        }
                    }
                });

                if changed {
                    cx.env.bind(key.0, scheme.clone());

                    // Update module member registry if applicable.
                    if let Some(mod_path) = key.1 {
                        if let Some(vis) =
                            cx.env.module_member_vis(&mod_path, key.0)
                        {
                            cx.env.register_user_module_member(
                                mod_path, key.0, scheme, vis,
                            );
                        }
                    }
                }
            }
        });
    }
}

/// A finalized function entry, recording metadata needed for retroactive
/// scheme enrichment of previously-finalized hoisted functions.
#[derive(Clone)]
struct FinalizedFun {
    name: StringId,
    vars: SmallVec<[TyVar; 4]>,
    /// Type variables from explicitly-declared type parameters (e.g. `[T, U]`).
    declared_tvs: HashSet<TyVar>,
    /// Declared type variable -> user-visible name (for error messages).
    tv_names: HashMap<TyVar, StringId>,
    mod_ctx: Option<QualifiedName>,
}

/// Borrowed `InferCtx` fields needed by `HoistState` methods.
pub(super) struct HoistCtx<'a> {
    pub(super) env: &'a mut TypeEnv,
    pub(super) uf: &'a mut UnionFind,
    pub(super) ty_arena: &'a mut TyArena,
    pub(super) constraints: &'a mut Vec<(Constraint, Option<QualifiedName>)>,
    pub(super) errors: &'a mut Vec<TypeError>,
    pub(super) current_module: &'a Option<QualifiedName>,
}

/// A type constraint generated during inference.
///
/// Constraints represent relationships between types that must hold for the
/// program to be well-typed. They are collected during the inference pass
/// and solved together via unification.
#[derive(Clone, Debug)]
pub(crate) enum Constraint {
    /// Two types must unify.
    ///
    /// Generated by assignments, function return types, binary operators
    /// requiring matching operand types, etc.
    ///
    /// Example: `let x: Int = e` generates `Unify(typeof(e), Int)`.
    ///
    /// Named `Unify` (not `Eq`) to disambiguate from `ClassId::EQ`.
    Unify(TyId, TyId, Span),

    /// Type must be callable with given argument types.
    ///
    /// Generated by function call expressions. During solving, the callee
    /// type is unified with `Fn(args, ret)`.
    ///
    /// Example: `f(x, y)` generates `Callable { callee: typeof(f), args: [typeof(x), typeof(y)], ret: ?r }`.
    Callable {
        callee: TyId,
        args: SmallVec<[TyId; 4]>,
        ret: TyId,
        span: Span,
    },

    /// Type must have a specific field.
    ///
    /// Generated by field access on type variables (`var.field`). Unlike
    /// unifying with `Object({field: T})`, this only requires the accessed
    /// field to exist; it doesn't require all object fields to be present.
    ///
    /// Example: `x.name` where `x: ?t` generates
    /// `HasField { base: ?t, field: "name", field_ty: ?f }`.
    HasField {
        base: TyId,
        field: intern::StringId,
        field_ty: TyId,
        span: Span,
    },

    /// Type must satisfy a type class.
    ///
    /// This is the unified representation for all class membership constraints.
    /// The `ty` field is the type being constrained, and `class` specifies
    /// which class it must belong to (with any associated types).
    ///
    /// Examples include `a + b` generating
    /// `Class { ty: typeof(a), class: Simple(Additive), span }`, `opt!`
    /// generating `Class { ty: typeof(opt), class: Hkt(Fallible, ?inner), span }`,
    /// and `arr[i]` generating
    /// `Class { ty: typeof(arr), class: Parameterized(Indexable, ?elem), span }`.
    Class {
        ty: TyId,
        class: TypeClass<TyId>,
        span: Span,
    },
}

impl Constraint {
    /// Get the source span associated with this constraint.
    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Unify(_, _, span)
            | Self::Callable { span, .. }
            | Self::HasField { span, .. }
            | Self::Class { span, .. } => *span,
        }
    }

    fn free_vars(&self, arena: &TyArena, uf: &mut UnionFind) -> HashSet<TyVar> {
        let mut vars = HashSet::new();
        match self {
            Self::Unify(a, b, _) => {
                vars.extend(uf.free_vars(*a, arena));
                vars.extend(uf.free_vars(*b, arena));
            }
            Self::Callable {
                callee, args, ret, ..
            } => {
                vars.extend(uf.free_vars(*callee, arena));
                args.iter().for_each(|arg| {
                    vars.extend(uf.free_vars(*arg, arena));
                });
                vars.extend(uf.free_vars(*ret, arena));
            }
            Self::HasField { base, field_ty, .. } => {
                vars.extend(uf.free_vars(*base, arena));
                vars.extend(uf.free_vars(*field_ty, arena));
            }
            Self::Class { ty, class, .. } => {
                vars.extend(uf.free_vars(*ty, arena));
                vars.extend(class.free_vars(arena, uf));
            }
        }
        vars
    }
}

/// Context for class instance type checking.
///
/// Tracks the current class being implemented and its associated type
/// definitions, enabling resolution of bare associated type references like
/// `:Index` within instance methods and associated type definitions.
#[derive(Clone, Debug)]
pub(crate) struct ClassContext {
    /// The class being implemented (e.g., `Indexable`).
    pub(crate) class: ClassId,
    /// The `TypeId` of the type this instance is for (e.g., `MyInt`).
    pub(crate) type_id: Option<TypeId>,
    /// Associated type definitions for this instance.
    ///
    /// Maps associated type names to their concrete types. For example,
    /// `newtype Index = Int` maps `"Index"` -> `TyArena::INT`.
    pub(crate) assoc_types: HashMap<StringId, TyId>,
}

type ReadCheck = (ExprId, TyId, TyId, Span, Option<QualifiedName>);
type NewtypeEdgeCheck = (ExprId, TyId, TyId, Span, Option<QualifiedName>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewtypeIntoOverlap {
    Public,
    Private,
}

/// Type inference context.
///
/// Collects type information during the inference pass: inferred expression
/// types, type variable bindings, and constraints. After inference completes,
/// constraints are solved via unification to produce final types.
pub(crate) struct InferCtx<'a> {
    /// Mutable AST for populating `TxnId` fields during typecheck.
    pub(super) ast: &'a mut ast::Ast,
    /// Registry of user-defined and builtin types.
    pub(super) registry: &'a TypeRegistry,
    /// AST-backed type declaration metadata used only while typechecking.
    pub(super) decls: TypeDeclRegistry,
    /// Runtime environment; used for builtin module typecheck setup.
    pub(super) runtime_env: &'a Environment,
    /// Scoped type environment (variable -> scheme bindings).
    pub(super) env: TypeEnv,
    /// Registry of user-defined class instances.
    ///
    /// Used during constraint solving to check if a user type satisfies
    /// a class constraint via a user-provided implementation.
    pub(super) instance_registry: InstanceRegistry,
    /// Type arena; owns all interned types.
    pub(super) ty_arena: TyArena,
    /// Collected constraints to be solved.
    constraints: Vec<(Constraint, Option<QualifiedName>)>,
    /// Union-find for type variable allocation and (future) constraint solving.
    pub(super) uf: UnionFind,
    /// Inferred types for each expression.
    expr_types: HashMap<ExprId, TyId>,
    /// Type errors encountered during inference.
    errors: Vec<TypeError>,
    /// Fields passed directly to the interpreter after inference.
    pub(super) interp: InterpreterOutput,
    /// Deferred instance call candidates to resolve after constraint solving.
    ///
    /// During inference, class method calls on types that are still type
    /// variables are recorded here. After `resolve_all_types`, we resolve the
    /// types and attach `ExprAux::InstanceCall` for any user instances found.
    deferred_inst_calls: Vec<(ExprId, TyId, ClassId)>,
    /// Deferred parameterized user class method calls.
    ///
    /// `(ExprId, ClassId, method, receiver_ty, class_arg_ty)`. After constraint
    /// solving, the class_arg_ty resolves to a concrete type; we look up the
    /// matching instance and attach its function name to expression metadata.
    deferred_param_calls: Vec<(ExprId, ClassId, StringId, TyId, TyId)>,
    /// Deferred user HKT class method calls.
    ///
    /// When multiple tuple instances exist for the same HKT class (e.g.,
    /// `MyMap for (T,)` and `MyMap for (T,U,)`), each call site gets the
    /// correct arity-specific function in expression metadata.
    deferred_hkt_user_calls: Vec<(ExprId, ClassId, StringId, TyId)>,
    /// Type variables created for integer literals, for defaulting to `Int`.
    ///
    /// Integer literals are polymorphic (no constraint) so they can unify with
    /// any type (including `Json` in heterogeneous arrays). After constraint
    /// solving, any unresolved type variables in this set default to `Int`.
    numeric_vars: Vec<TyVar>,
    /// Current transaction ID, if inside a `transaction` block.
    ///
    /// Used to enforce that global writes (`@set ^...`, `@kill ^...`) only
    /// appear inside `transaction { ... }` blocks, that nested `transaction`s
    /// cannot be created (not supported), and to populate `TxnId` fields in
    /// the AST for DB operations.
    pub(super) in_transaction: Option<TxnId>,
    /// Counter for generating unique `TxnId` values.
    next_txn_id: u32,
    /// Current class context, if inside a `class ... FOR ...` instance.
    ///
    /// Set when processing class instance methods and associated type
    /// definitions. Enables resolution of bare associated type references
    /// like `:Index` to the concrete types defined in the current instance.
    pub(super) class_context: Option<ClassContext>,
    /// Active type parameter substitutions for type expressions inside
    /// generic function, closure, and instance method bodies.
    pub(super) ty_substs: Vec<IndexMap<StringId, TyId>>,
    /// Current module path during typechecking (e.g., `Math.Vector`).
    ///
    /// `None` when at top-level; `Some(qn)` inside a module.
    /// Used to resolve unqualified type names within modules.
    pub(super) current_module: Option<QualifiedName>,
    /// Whether running in interactive mode (no `main` required).
    ///
    /// In interactive mode, top-level expression statements are allowed and
    /// executed sequentially. In normal mode, a `main` function is required
    /// and top-level expressions are rejected.
    interactive: bool,
    /// Type variables representing polymorphic parameters.
    ///
    /// When entering a function body with type parameters (e.g., `[T, F: Fallible[T]]`),
    /// the fresh type variables created for those parameters are added here. These
    /// represent universally quantified types that cannot be refined by pattern matching.
    ///
    /// Contrast with inference variables (from method calls, etc.) which are NOT
    /// in this set and CAN be pattern-matched since they will unify to concrete types.
    pub(super) poly_param_vars: HashSet<TyVar>,
    /// Declared type parameter names and constraints for the current `let` RHS.
    ///
    /// A `let` bound callable with explicit type parameters may capture
    /// declared constraints for those parameters, but inferred constraints
    /// must be reported as missing declarations.
    pub(super) let_tv_frames: Vec<LetTvFrame>,
    /// Recorded `let` annotations for post-solve union narrowing validation.
    ///
    /// Each entry is `(rhs_expr, rhs_ty, ann_ty, span)`. After constraint
    /// solving resolves type variables, these are checked: if `rhs_ty`
    /// resolved to a union and `ann_ty` did not, the annotation illegally
    /// narrows a union type. Union annotations also record the resolved member
    /// representation for runtime metadata.
    let_annotations: Vec<(ExprId, TyId, TyId, Span)>,
    /// Recorded `read` conversions for checked runtime metadata.
    read_checks: Vec<ReadCheck>,
    /// Recorded newtype representation edges for checked runtime metadata.
    newtype_edge_checks: Vec<NewtypeEdgeCheck>,
    /// Hoisting and forward-reference tracking state.
    pub(super) hoist: HoistState,
    /// Whether unresolved user-module value imports should be replayed later.
    defer_missing_import_members: bool,
    /// User-module value imports deferred until module `let`s are finalized.
    deferred_imports: Vec<(ast::Import, Span, Option<QualifiedName>)>,
}

impl<'a> InferCtx<'a> {
    /// Create a new inference context.
    ///
    /// The `strings` interner should be shared with the `TypeRegistry` so
    /// type name lookups produce consistent `StringId`s. The `runtime_env`
    /// provides builtin module setup data.
    /// Set `interactive` to `true` to allow top-level expressions without
    /// requiring a `main` function.
    pub(crate) fn new(
        ast: &'a mut ast::Ast,
        registry: &'a TypeRegistry,
        runtime_env: &'a Environment,
        strings: StringInterner,
        interactive: bool,
    ) -> Self {
        // Snapshot the runtime environment's arena so that `TyId`s from
        // builtin module schemes remain valid; inference will extend this
        // copy with its own type allocations.
        let mut ty_arena = runtime_env.ty_arena.clone();
        let env = TypeEnv::new(strings, &mut ty_arena);
        Self {
            ast,
            registry,
            decls: TypeDeclRegistry::default(),
            runtime_env,
            env,
            instance_registry: InstanceRegistry::new(),
            ty_arena,
            constraints: Vec::new(),
            uf: UnionFind::new(),
            expr_types: HashMap::new(),
            errors: Vec::new(),
            interp: InterpreterOutput::new(),
            deferred_inst_calls: Vec::new(),
            deferred_param_calls: Vec::new(),
            deferred_hkt_user_calls: Vec::new(),
            numeric_vars: Vec::new(),
            in_transaction: None,
            next_txn_id: 0,
            class_context: None,
            ty_substs: Vec::new(),
            current_module: None,
            interactive,
            poly_param_vars: HashSet::new(),
            let_tv_frames: Vec::new(),
            let_annotations: Vec::new(),
            read_checks: Vec::new(),
            newtype_edge_checks: Vec::new(),
            hoist: HoistState::new(),
            defer_missing_import_members: false,
            deferred_imports: Vec::new(),
        }
    }

    pub(super) fn push_let_tv_frame(&mut self) {
        self.let_tv_frames.push(LetTvFrame::default());
    }

    pub(super) fn pop_let_tv_frame(&mut self) {
        self.let_tv_frames.pop();
    }

    pub(super) fn record_let_tv_name(&mut self, tv: TyVar, name: StringId) {
        if let Some(fr) = self.let_tv_frames.last_mut() {
            fr.names.insert(tv, name);
        }
    }

    pub(super) fn record_let_tv_constraint(
        &mut self,
        tv: TyVar,
        class: TypeClass<TyId>,
    ) {
        if let Some(fr) = self.let_tv_frames.last_mut() {
            fr.cs.push((tv, class));
        }
    }

    pub(super) fn current_let_tv_frame(&self) -> Option<&LetTvFrame> {
        self.let_tv_frames.last()
    }

    /// Compile a regex pattern, caching it and returning the cache index.
    ///
    /// If the pattern is invalid, records a type error and returns `None`.
    pub(crate) fn compile_regex(
        &mut self,
        pattern: &str,
        span: Span,
    ) -> Option<u32> {
        match regex::Regex::new(pattern) {
            Ok(r) => {
                let idx = self.interp.regex_cache.len() as u32;
                self.interp.regex_cache.push(r);
                Some(idx)
            }
            Err(e) => {
                self.error(TypeError::InvalidRegex(
                    pattern.to_string(),
                    e.to_string(),
                    span,
                ));
                None
            }
        }
    }

    /// Generate a fresh type variable.
    pub(crate) fn fresh_var(&mut self) -> TyVar {
        self.uf.fresh()
    }

    /// Generate a fresh type variable wrapped in `Ty::Var`, interned.
    pub(crate) fn fresh(&mut self) -> TyId {
        let v = self.fresh_var();
        self.ty_arena.alloc(Ty::Var(v))
    }

    /// Generate a fresh type variable for an integer literal.
    ///
    /// Unlike `fresh`, this does NOT emit a `Numeric` constraint. The type
    /// variable can unify with any type (including `Json` in heterogeneous
    /// arrays). Unresolved numeric type vars default to `Int` after solving.
    pub(crate) fn fresh_numeric(&mut self) -> TyId {
        let v = self.fresh_var();
        self.numeric_vars.push(v);
        self.ty_arena.alloc(Ty::Var(v))
    }

    /// Add a constraint to the collection.
    pub(crate) fn constrain(&mut self, c: Constraint) {
        self.constraints.push((c, self.current_module.clone()));
    }

    /// Add a unification constraint between two types.
    ///
    /// Shorthand for `constrain(Constraint::Unify(t1, t2, span))`.
    pub(crate) fn unify(&mut self, t1: TyId, t2: TyId, span: Span) {
        self.constrain(Constraint::Unify(t1, t2, span));
    }

    /// Emit constraints from class constraints.
    ///
    /// Called after instantiating a scheme to re-emit the constraints with
    /// the fresh type variables. This ensures constraints are checked at
    /// call sites, not just at function definition.
    pub(crate) fn emit_class_constraints(
        &mut self,
        constraints: smallvec::SmallVec<[(TyId, TypeClass<TyId>); 2]>,
        span: Span,
    ) {
        constraints.into_iter().for_each(|(ty, class)| {
            self.constrain(Constraint::Class { ty, class, span });
        });
    }

    /// Record the inferred type for an expression.
    pub(crate) fn record_type(&mut self, id: ExprId, ty: TyId) {
        self.expr_types.insert(id, ty);
    }

    /// Record a type error.
    pub(crate) fn error(&mut self, e: TypeError) {
        self.errors.push(e);
    }

    /// Get the AST being type-checked.
    pub(crate) fn ast(&self) -> &ast::Ast {
        self.ast
    }

    /// Get the type registry.
    pub(crate) fn registry(&self) -> &TypeRegistry {
        self.registry
    }

    /// Get a mutable reference to the type environment.
    pub(crate) fn env_mut(&mut self) -> &mut TypeEnv {
        &mut self.env
    }

    /// Get an immutable reference to the type environment.
    pub(crate) fn env(&self) -> &TypeEnv {
        &self.env
    }

    /// Get the collected constraints.
    pub(crate) fn constraints(&self) -> Vec<&Constraint> {
        self.constraints.iter().map(|(c, _)| c).collect()
    }

    /// Get the inferred type for an expression, if recorded.
    pub(crate) fn get_type(&self, id: ExprId) -> Option<TyId> {
        self.expr_types.get(&id).copied()
    }

    /// Get all expression types.
    pub(crate) fn expr_types(&self) -> &HashMap<ExprId, TyId> {
        &self.expr_types
    }

    /// Check if any errors were recorded.
    pub(crate) fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Get the collected errors.
    pub(crate) fn errors(&self) -> &[TypeError] {
        &self.errors
    }

    /// Take ownership of collected errors, leaving an empty vec.
    pub(crate) fn take_errors(&mut self) -> Vec<TypeError> {
        mem::take(&mut self.errors)
    }

    /// Build a type-parameter substitution from an instance definition.
    ///
    /// Maps instance type params to caller-supplied type args.
    /// Var params become entries in the returned `Rename`; concrete params
    /// are unified via deferred constraints.
    pub(super) fn build_instance_subst(
        &mut self,
        inst: &Instance,
        type_args: &[TyId],
        span: Span,
    ) -> Rename {
        let (vars, concretes): (
            SmallVec<[(TyVar, TyId); 2]>,
            SmallVec<[(TyId, TyId); 2]>,
        ) = inst.type_params.iter().zip(type_args.iter()).fold(
            (SmallVec::new(), SmallVec::new()),
            |(mut vs, mut cs), (&p, &a)| {
                match self.ty_arena.get(p) {
                    Ty::Var(tv) => vs.push((*tv, a)),
                    _ => cs.push((p, a)),
                }
                (vs, cs)
            },
        );
        concretes.into_iter().for_each(|(p, a)| {
            self.unify(p, a, span);
        });
        Rename(vars.into_iter().collect())
    }

    /// Create a `SolveCtx` and run constraint solving.
    ///
    /// Takes ownership of constraints, then delegates to
    /// `SolveCtx::solve_constraints`.
    fn solve(&mut self) {
        let constraints = mem::take(&mut self.constraints);
        SolveCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            decls: &self.decls,
            instance_registry: &self.instance_registry,
            env: &self.env,
            errors: &mut self.errors,
            ast: self.ast,
            current_module: self.current_module.clone(),
            class_context: &self.class_context,
            hkt_var_classes: HashMap::new(),
        }
        .solve_constraints(constraints, &self.numeric_vars);
    }

    pub(super) fn newtype_edge_status(
        &mut self,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> NewtypeEdgeStatus {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let st = SolveCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            decls: &self.decls,
            instance_registry: &self.instance_registry,
            env: &self.env,
            errors: &mut self.errors,
            ast: self.ast,
            current_module: module,
            class_context: &self.class_context,
            hkt_var_classes: HashMap::new(),
        }
        .newtype_edge_status(from, to, span);
        self.uf.rollback(snap);
        self.errors.truncate(err_len);
        st
    }

    pub(super) fn newtype_edge_overlaps_into(
        &mut self,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> Option<NewtypeIntoOverlap> {
        self.any_newtype_edge(from, to, module, span).map(|edge| {
            match self.decls.alias_repr_vis(edge.alias) {
                ast::Visibility::Public => NewtypeIntoOverlap::Public,
                ast::Visibility::Private => NewtypeIntoOverlap::Private,
            }
        })
    }

    pub(super) fn private_try_into_external(
        &mut self,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> bool {
        self.any_newtype_edge(from, to, module.clone(), span)
            .is_some_and(|edge| {
                self.decls.alias_repr_vis(edge.alias)
                    == ast::Visibility::Private
                    && !is_in_module(
                        &module,
                        &self.decls.alias_module(edge.alias).cloned(),
                    )
            })
    }

    pub(super) fn reject_private_repr_ann(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> bool {
        if self.newtype_edge_status(from, to, self.current_module.clone(), span)
            == NewtypeEdgeStatus::Blocked
        {
            self.error(TypeError::PrivateReprAnnotation { from, to, span });
            true
        } else {
            false
        }
    }

    fn approved_newtype_edge(
        &mut self,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> Option<NewtypeEdge> {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let edge = SolveCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            decls: &self.decls,
            instance_registry: &self.instance_registry,
            env: &self.env,
            errors: &mut self.errors,
            ast: self.ast,
            current_module: module,
            class_context: &self.class_context,
            hkt_var_classes: HashMap::new(),
        }
        .newtype_edge(from, to, span);
        self.uf.rollback(snap);
        self.errors.truncate(err_len);
        edge
    }

    fn any_newtype_edge(
        &mut self,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> Option<NewtypeEdge> {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let edge = SolveCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            decls: &self.decls,
            instance_registry: &self.instance_registry,
            env: &self.env,
            errors: &mut self.errors,
            ast: self.ast,
            current_module: module,
            class_context: &self.class_context,
            hkt_var_classes: HashMap::new(),
        }
        .newtype_edge_any(from, to, span);
        self.uf.rollback(snap);
        self.errors.truncate(err_len);
        edge
    }

    fn record_approved_newtype_edge(
        &mut self,
        id: ExprId,
        from: TyId,
        to: TyId,
        module: Option<QualifiedName>,
        span: Span,
    ) -> bool {
        self.approved_newtype_edge(from, to, module, span)
            .is_some_and(|edge| {
                let from = self.uf.resolve(edge.from, &mut self.ty_arena);
                let to = self.uf.resolve(edge.to, &mut self.ty_arena);
                let repr = self.uf.resolve(edge.repr, &mut self.ty_arena);
                self.interp.approved_newtype_edges.insert(
                    id,
                    CheckedNewtypeEdgeRuntimeInfo { from, to, repr },
                );
                true
            })
    }

    /// Resolve all inferred types through the union-find.
    ///
    /// Called after constraint solving to replace type variables with their
    /// resolved concrete types.
    pub(crate) fn resolve_all_types(&mut self) {
        self.uf
            .resolve_map(&mut self.expr_types, &mut self.ty_arena);
        self.interp.resolve(&mut self.uf, &mut self.ty_arena);
        self.collect_runtime_alias_expansions();
    }

    fn collect_runtime_alias_expansions(&mut self) {
        let tys: Vec<TyId> = self
            .expr_types
            .values()
            .copied()
            .chain(
                self.interp
                    .expr_metadata
                    .values()
                    .flat_map(CheckedExprInfo::ty_ids),
            )
            .chain(self.interp.union_value_reprs.values().copied())
            .chain(self.interp.expr_targets.values().copied())
            .chain(
                self.interp
                    .approved_newtype_edges
                    .values()
                    .flat_map(CheckedNewtypeEdgeRuntimeInfo::ty_ids),
            )
            .chain(
                self.interp
                    .is_patterns
                    .values()
                    .flat_map(CheckedTypePatternInfo::ty_ids),
            )
            .chain(self.interp.let_targets.values().copied())
            .chain(self.interp.match_targets.values().copied())
            .collect();

        tys.into_iter().for_each(|ty| {
            self.collect_alias_expansions(ty, &mut HashSet::new())
        });
    }

    fn checked_expr_type_id(&mut self, tid: TypeId) -> TyId {
        self.ty_arena.named(tid, SmallVec::new())
    }

    fn set_instance_call(&mut self, expr: ExprId, tid: TypeId) {
        let ty = self.checked_expr_type_id(tid);
        self.interp.set_instance_call(expr, ty);
    }

    fn type_id_args(&self, ty: TyId) -> Option<(TypeId, SmallVec<[TyId; 4]>)> {
        match self.ty_arena.get(ty).clone() {
            Ty::Named(id, args) => Some((id, args)),
            Ty::Union(Some(id), _) => Some((id, SmallVec::new())),
            Ty::Bool => Some((TypeId::BOOL, SmallVec::new())),
            Ty::Int => Some((TypeId::INT, SmallVec::new())),
            Ty::Word => Some((TypeId::WORD, SmallVec::new())),
            Ty::Float => Some((TypeId::FLOAT, SmallVec::new())),
            Ty::Char => Some((TypeId::CHAR, SmallVec::new())),
            Ty::String => Some((TypeId::STRING, SmallVec::new())),
            Ty::Unit => Some((TypeId::UNIT, SmallVec::new())),
            Ty::Time => Some((TypeId::TIME, SmallVec::new())),
            Ty::Range => Some((TypeId::RANGE, SmallVec::new())),
            Ty::Json => Some((TypeId::JSON, SmallVec::new())),
            Ty::Ordering => Some((TypeId::ORDERING, SmallVec::new())),
            Ty::DataStatus => Some((TypeId::DATA_STATUS, SmallVec::new())),
            Ty::FilePath => Some((TypeId::FILEPATH, SmallVec::new())),
            Ty::Path => Some((TypeId::PATH, SmallVec::new())),
            Ty::Regex => Some((TypeId::REGEX, SmallVec::new())),
            Ty::Local => Some((TypeId::LOCAL, SmallVec::new())),
            Ty::Global => Some((TypeId::GLOBAL, SmallVec::new())),
            Ty::Array(e) => Some((TypeId::ARRAY, [e].into_iter().collect())),
            Ty::Option(e) => Some((TypeId::OPTION, [e].into_iter().collect())),
            Ty::Result(ok, err) => {
                Some((TypeId::RESULT, [ok, err].into_iter().collect()))
            }
            Ty::Map(k, v) => Some((TypeId::MAP, [k, v].into_iter().collect())),
            Ty::Tuple(args) => Some((TypeId::TUPLE, args)),
            Ty::Var(_)
            | Ty::Fn(_, _)
            | Ty::Object(_)
            | Ty::Union(None, _)
            | Ty::RuntimeError
            | Ty::Apply(_, _)
            | Ty::AssocType(_, _, _)
            | Ty::Unknown
            | Ty::Error => None,
        }
    }

    fn build_inst_subst_read(&self, inst: &Instance, args: &[TyId]) -> Rename {
        let vars: SmallVec<[(TyVar, TyId); 2]> = inst
            .type_params
            .iter()
            .zip(args.iter())
            .filter_map(|(&p, &a)| match self.ty_arena.get(p) {
                Ty::Var(tv) => Some((*tv, a)),
                _ => None,
            })
            .collect();
        Rename(vars.into_iter().collect())
    }

    fn read_try_inst(
        &mut self,
        from: TyId,
        to: TyId,
    ) -> Option<(TypeId, Instance)> {
        let (tid, args) = self.type_id_args(from)?;
        let insts: Vec<Instance> = self
            .instance_registry
            .lookup_all(ClassId::TRY_INTO, tid)
            .to_vec();
        let to = self.uf.resolve(to, &mut self.ty_arena);
        insts
            .into_iter()
            .find(|inst| {
                inst.class_args.first().is_some_and(|&ia| {
                    let subst = self.build_inst_subst_read(inst, &args);
                    let resolved = self.ty_arena.apply(ia, &subst);
                    self.uf.resolve(resolved, &mut self.ty_arena) == to
                })
            })
            .map(|inst| (tid, inst))
    }

    pub(crate) fn resolve_read_metadata(&mut self) {
        let reads = mem::take(&mut self.read_checks);
        reads.into_iter().for_each(|(id, from, to, _, _)| {
            let from = self.uf.resolve(from, &mut self.ty_arena);
            let to = self.uf.resolve(to, &mut self.ty_arena);
            if from == to {
                self.expand_alias_for_read(to);
            } else if let Some((tid, inst)) = self.read_try_inst(from, to) {
                let method = self.env.intern("try-into");
                self.expand_alias_for_read(to);
                self.set_instance_call(id, tid);
                inst.methods.get(&method).copied().into_iter().for_each(
                    |fun| {
                        self.interp.set_instance_fun(id, fun);
                    },
                );
            }
        });
    }

    pub(crate) fn resolve_newtype_edge_metadata(&mut self) {
        let checks = mem::take(&mut self.newtype_edge_checks);
        checks.into_iter().for_each(|(id, from, to, span, module)| {
            let from = self.uf.resolve(from, &mut self.ty_arena);
            let to = self.uf.resolve(to, &mut self.ty_arena);
            if from == to {
            } else {
                self.record_approved_newtype_edge(id, from, to, module, span);
            }
        });
    }

    fn metadata_has_unresolved(&self, info: &CheckedExprInfo) -> bool {
        info.concrete
            && info.ty.is_some_and(|ty| {
                matches!(self.ty_arena.get(ty), Ty::Var(_) | Ty::Unknown)
            })
    }

    fn missing_annotation_ids(&self) -> Vec<ExprId> {
        let expr_ids: HashSet<ExprId> = self
            .expr_types
            .iter()
            .filter(|(_, &ty)| Self::has_unresolved_vars(ty, &self.ty_arena))
            .map(|(&id, _)| id)
            .collect();
        let meta_ids = self
            .interp
            .expr_metadata
            .iter()
            .filter(|(_, info)| self.metadata_has_unresolved(info))
            .filter(|(&id, _)| !expr_ids.contains(&id))
            .map(|(&id, _)| id)
            .collect::<Vec<_>>();
        expr_ids.into_iter().chain(meta_ids).collect()
    }

    /// Resolve deferred instance dispatch after constraint solving.
    ///
    /// After constraint solving and type resolution, type variables are resolved
    /// to concrete types. This method iterates through deferred dispatch
    /// candidates, resolves their types, and attaches call metadata for any
    /// that have user-defined instances.
    pub(crate) fn resolve_deferred_inst_calls(&mut self) {
        // Take ownership to avoid borrow issues
        let deferred = mem::take(&mut self.deferred_inst_calls);

        deferred.into_iter().for_each(|(expr_id, ty, kind)| {
            let resolved = self.uf.resolve(ty, &mut self.ty_arena);
            let type_id = match self.ty_arena.get(resolved) {
                Ty::Named(id, _) | Ty::Union(Some(id), _) => Some(*id),
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
                Ty::Tuple(_) => Some(TypeId::TUPLE),
                _ => None,
            };

            if let Some(tid) = type_id {
                if self.instance_registry.lookup(kind, tid).is_some() {
                    self.set_instance_call(expr_id, tid);
                }
            }
        });
    }

    /// Resolve deferred parameterized user class method calls.
    ///
    /// After constraint solving, the class_arg type is concrete. We look up
    /// the matching instance and record the specific generated function name
    /// so the runtime can dispatch correctly when multiple instances of the
    /// same parameterized class exist for one type.
    pub(crate) fn resolve_deferred_param_calls(&mut self) {
        let deferred = mem::take(&mut self.deferred_param_calls);
        deferred
            .into_iter()
            .for_each(|(eid, cid, method, recv, ca)| {
                let recv_r = self.uf.resolve(recv, &mut self.ty_arena);
                let ca_r = self.uf.resolve(ca, &mut self.ty_arena);
                let tid = match self.ty_arena.get(recv_r) {
                    Ty::Named(id, _) | Ty::Union(Some(id), _) => Some(*id),
                    _ => None,
                };
                if let Some(tid) = tid {
                    let insts = self.instance_registry.lookup_all(cid, tid);
                    // Only need disambiguation when multiple instances exist
                    if insts.len() > 1 {
                        insts
                            .iter()
                            .find(|i| {
                                i.class_args.first().copied() == Some(ca_r)
                            })
                            .and_then(|i| i.methods.get(&method).copied())
                            .into_iter()
                            .for_each(|fn_id| {
                                self.interp.set_instance_fun(eid, fn_id);
                            });
                    }
                }
            });
    }

    /// Resolve deferred user HKT class calls.
    ///
    /// When multiple tuple instances exist for the same class, each
    /// call site must be pre-resolved to the arity-specific function.
    pub(crate) fn resolve_deferred_hkt_user_calls(&mut self) {
        let deferred = mem::take(&mut self.deferred_hkt_user_calls);
        deferred.into_iter().for_each(|(eid, cid, method, ty)| {
            let resolved = self.uf.resolve(ty, &mut self.ty_arena);
            if let Ty::Tuple(ts) = self.ty_arena.get(resolved).clone() {
                let insts =
                    self.instance_registry.lookup_all(cid, TypeId::TUPLE);
                if insts.len() > 1 {
                    self.instance_registry
                        .lookup_tuple(cid, ts.len())
                        .and_then(|i| i.methods.get(&method).copied())
                        .into_iter()
                        .for_each(|fn_id| {
                            self.interp.set_instance_fun(eid, fn_id);
                        });
                }
            }
        });
    }

    /// Check for illegal union narrowing in `let` annotations.
    ///
    /// After constraint solving, if a `let` binding's RHS resolved to a
    /// union type but the annotation is not a union, the user is attempting
    /// to narrow a union via annotation. This requires `match`/`is` instead.
    pub(crate) fn check_let_union_narrowing(&mut self) {
        mem::take(&mut self.let_annotations).into_iter().for_each(
            |(expr, rhs, ann, span)| {
                let rhs = self.uf.resolve(rhs, &mut self.ty_arena);
                let ann = self.uf.resolve(ann, &mut self.ty_arena);
                if self.reject_iterable_reverse_union_ann(expr, rhs, ann) {
                    self.errors.push(TypeError::Mismatch {
                        expected: ann,
                        got: rhs,
                        span,
                    });
                } else if matches!(self.ty_arena.get(rhs), Ty::Union(..))
                    && !matches!(
                        self.ty_arena.get(ann),
                        Ty::Union(..) | Ty::Error
                    )
                {
                    self.errors.push(TypeError::UnionNarrowing {
                        union_ty: rhs,
                        narrow_ty: ann,
                        span,
                    });
                }
                if let Some(member) = self.union_member_repr(rhs, ann) {
                    self.interp.union_value_reprs.insert(expr, member);
                }
            },
        );
    }

    fn reject_iterable_reverse_union_ann(
        &self,
        expr: ExprId,
        rhs: TyId,
        ann: TyId,
    ) -> bool {
        self.is_iterable_reverse_call(expr)
            && !matches!(self.ty_arena.get(rhs), Ty::Union(..) | Ty::Error)
            && matches!(self.ty_arena.get(ann), Ty::Union(None, ms) if ms.len() > 1)
    }

    fn is_iterable_reverse_call(&self, expr: ExprId) -> bool {
        self.ast.get_expr(expr).is_some_and(|e| match e {
            ast::Expr::ClassMethod(class, method, args) => {
                args.len() == 1
                    && self.env.resolve_str(*class) == "Iterable"
                    && self.env.resolve_str(*method) == "reverse"
            }
            _ => false,
        })
    }

    /// After constraint solving and substitution application, any remaining
    /// `Ty::Var` or `Ty::Unknown` indicates incomplete inference. This emits
    /// `MissingAnnotation` errors for such cases.
    pub(crate) fn check_remaining_unknowns(&mut self) {
        self.missing_annotation_ids()
            .into_iter()
            .map(|id| self.ast.expr_span(id).unwrap_or_else(|| Span::new(0, 0)))
            .for_each(|span| {
                self.errors.push(TypeError::MissingAnnotation(span));
            });
    }

    /// Validate that the script has a valid `main` entry point.
    ///
    /// In non-interactive mode:
    /// - Top-level `Stmt::Expr` is rejected (must be inside `main`)
    /// - A `main` function must exist
    /// - `main` must have signature `() -> Unit`
    ///
    /// In interactive mode, this validation is skipped entirely.
    pub(crate) fn validate_main_entry_point(&mut self, stmts: &[StmtId]) {
        if !self.interactive {
            // Reject top-level expression statements
            stmts.iter().for_each(|id| {
                if let Some(Stmt::Expr(_)) = self.ast.get_stmt(*id) {
                    let span = self
                        .ast
                        .stmt_span(*id)
                        .unwrap_or_else(|| Span::new(0, 0));
                    self.errors.push(TypeError::TopLevelExpr(span));
                }
            });

            // Check for `main` function
            let file_span = Span::new(0, 0);
            let expected =
                self.ty_arena.func(smallvec::smallvec![], TyArena::UNIT);
            let main_id = self.env.intern("main");
            let main_err = self.env.lookup(main_id).map_or_else(
                || Some(TypeError::MissingMain(file_span)),
                |scheme| {
                    // Resolve through union-find; the scheme's `ty` may
                    // contain unresolved vars (e.g. from `Callable`
                    // constraints on the last expression in `main`)
                    let resolved =
                        self.uf.resolve(scheme.ty, &mut self.ty_arena);
                    if resolved == expected {
                        None
                    } else {
                        Some(TypeError::InvalidMainSignature {
                            got: resolved,
                            span: file_span,
                        })
                    }
                },
            );
            main_err.into_iter().for_each(|e| self.errors.push(e));
        };
    }

    /// Run type checking on the given statements.
    ///
    /// Performs type inference, constraint solving, and validation. Returns
    /// `TypecheckOutput` on success, or formatted type errors on failure.
    pub(crate) fn check(
        mut self,
        stmts: &[StmtId],
        registry: &TypeRegistry,
        arena: &value::ValueArena,
    ) -> Result<TypecheckOutput> {
        self.decls = TypeDeclRegistry::from_ast(self.ast, stmts, self.registry);

        // Pass 1: Hoist function and module declarations for forward references
        self.hoist_declarations(stmts);

        // Pass `2`: Infer types for all statement bodies.
        self.install_final_let_schemes(stmts);
        stmts.iter().copied().for_each(|id| self.stmt(id));

        // Solve collected constraints (updates union-find in-place)
        self.solve();

        // Enable zonk cache for resolution passes (bindings are frozen
        // post-solve, so memoization is safe and avoids redundant tree walks)
        self.uf.enable_zonk_cache();

        // Resolve all type variables through the union-find
        self.resolve_all_types();

        // Resolve deferred instance dispatch (now that types are resolved)
        self.resolve_deferred_inst_calls();

        // Resolve parameterized user class method call function names
        self.resolve_deferred_param_calls();

        // Resolve deferred user HKT class calls (arity-based disambiguation)
        self.resolve_deferred_hkt_user_calls();

        // Record `read` target metadata only after validation succeeds.
        self.resolve_read_metadata();

        // Record approved newtype edges only after validation succeeds.
        self.resolve_newtype_edge_metadata();

        self.uf.disable_zonk_cache();

        // Check for illegal union narrowing via let annotations
        self.check_let_union_narrowing();

        self.collect_literal_union_reprs();

        // Check for remaining unresolved type variables
        self.check_remaining_unknowns();

        // Validate main entry point (in non-interactive mode)
        self.validate_main_entry_point(stmts);

        self.into_output(registry, arena)
    }

    fn collect_literal_union_reprs(&mut self) {
        let exprs: Vec<_> =
            self.expr_types.iter().map(|(&id, &ty)| (id, ty)).collect();
        exprs.into_iter().for_each(|(id, ty)| {
            if matches!(self.ty_arena.get(ty), Ty::Union(_, _)) {
                if let Some(member) = self.expr_union_member(id, ty) {
                    self.interp.union_value_reprs.insert(id, member);
                }
            }
        });
    }

    fn expr_union_member(&self, id: ExprId, union: TyId) -> Option<TyId> {
        self.ast.get_expr(id).and_then(|expr| match expr {
            ast::Expr::Literal(lit) => self.literal_union_member(lit, union),
            ast::Expr::Annotate(inner, _) => {
                self.expr_union_member(*inner, union)
            }
            ast::Expr::Json(_) => {
                self.union_member_matching(union, TyArena::JSON)
            }
            _ => None,
        })
    }

    fn literal_union_member(
        &self,
        lit: &ast::Literal,
        union: TyId,
    ) -> Option<TyId> {
        match lit {
            ast::Literal::Bool(_) => {
                self.union_member_matching(union, TyArena::BOOL)
            }
            ast::Literal::Numeric(ast::NumericLit::Int(_)) => {
                [TyArena::INT, TyArena::WORD, TyArena::FLOAT]
                    .into_iter()
                    .find_map(|ty| self.union_member_matching(union, ty))
            }
            ast::Literal::Numeric(ast::NumericLit::Float(_)) => {
                self.union_member_matching(union, TyArena::FLOAT)
            }
            ast::Literal::Char(_) => {
                self.union_member_matching(union, TyArena::CHAR)
            }
            ast::Literal::String(_) => {
                self.union_member_matching(union, TyArena::STRING)
            }
            ast::Literal::Null => {
                self.union_member_matching(union, TyArena::JSON)
            }
            ast::Literal::Unit => {
                self.union_member_matching(union, TyArena::UNIT)
            }
        }
    }

    fn union_member_matching(
        &self,
        union: TyId,
        candidate: TyId,
    ) -> Option<TyId> {
        match self.ty_arena.get(union) {
            Ty::Union(_, members) => members
                .iter()
                .copied()
                .find(|member| self.types_compatible(candidate, *member)),
            _ => None,
        }
    }

    /// Consume the context, returning `TypecheckOutput` on success or
    /// formatted type errors on failure.
    fn into_output(
        self,
        registry: &TypeRegistry,
        val_arena: &value::ValueArena,
    ) -> Result<TypecheckOutput> {
        if let Some(errs) = NonEmpty::from_vec(self.errors) {
            let printer = TyPrinter::new(
                registry,
                val_arena,
                &self.ty_arena,
                &self.env.strings,
                self.env.class_registry(),
                &self.numeric_vars,
            );
            let formatted = errs.map(|e| e.format_with(&printer));
            let errors = formatted.map(Error::FormattedType);
            Err(Error::multiple(errors))
        } else {
            let module_fn_types = self
                .runtime_env
                .builtin_module_fn_types()
                .into_iter()
                .map(|(path, scheme)| (path, scheme.ty))
                .collect();
            let module_const_types =
                self.runtime_env.builtin_module_const_types();
            Ok(TypecheckOutput {
                ty_arena: self.ty_arena,
                regex_cache: self.interp.regex_cache,
                expr_metadata: self.interp.expr_metadata,
                union_value_reprs: self.interp.union_value_reprs,
                function_types: self.interp.function_types,
                module_fn_types,
                module_const_types,
                class_registry: self.env.class_registry,
                expr_types: self.expr_types,
                expr_targets: self.interp.expr_targets,
                approved_newtype_edges: self.interp.approved_newtype_edges,
                is_patterns: self.interp.is_patterns,
                let_targets: self.interp.let_targets,
                match_targets: self.interp.match_targets,
                alias_type_expansions: self.interp.alias_type_expansions,
            })
        }
    }

    pub(super) fn cur_subst(&self) -> IndexMap<StringId, TyId> {
        let mut subst = IndexMap::new();
        self.ty_substs.iter().for_each(|s| {
            s.iter().for_each(|(&name, &ty)| {
                subst.insert(name, ty);
            });
        });
        subst
    }

    pub(super) fn ast_ty(&mut self, id: AstTypeExprId) -> TyId {
        let subst = self.cur_subst();
        self.convert().ast_type_to_ty(id, &subst)
    }
}
