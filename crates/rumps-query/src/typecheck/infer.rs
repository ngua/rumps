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

mod convert;
mod expr;
mod hoist;
mod pattern;
mod stmt;

use std::collections::{HashMap, HashSet};
use std::mem;
use std::ops::Range;

use indexmap::IndexMap;
use nonempty::NonEmpty;
use smallvec::SmallVec;

use super::env::TypeEnv;
use super::error::{TyPrinter, TypeError};
use super::instance::InstanceRegistry;
use super::ty::{Scheme, Ty, TyArena, TyId, TyVar, TypeClass};
use super::uf::UnionFind;
use super::TypecheckOutput;
use crate::ast::{
    self, AssocTypeDef, AstClassConstraints, AstTypeExprId, ExprId,
    InstanceMethodDef, Stmt, StmtId, TxnId, TypeParam,
};
use crate::env::Environment;
use crate::error::Result;
use crate::intern::{self, QualifiedName, StringId, StringInterner};
use crate::value::{self, TypeExprArena, TypeId, TypeRegistry};
use crate::{ClassId, Error, Span};

/// Resolve all `TyId` values in a map through the union-find.
fn resolve_map(
    map: &mut HashMap<ExprId, TyId>,
    uf: &mut UnionFind,
    arena: &mut TyArena,
) {
    map.values_mut().for_each(|ty| *ty = uf.resolve(*ty, arena));
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
    /// Mapping from regex expression IDs to cache indices.
    ///
    /// When interpreting an `Expr::Regex`, look up the cache index here.
    pub(super) regex_indices: HashMap<ExprId, u32>,
    /// Mapping from mempty expression IDs to their inferred types.
    ///
    /// Populated during inference with type variables; resolved after
    /// substitution to concrete `Monoid` types. The interpreter uses
    /// this to produce the correct empty value.
    pub(super) mempty_types: HashMap<ExprId, TyId>,
    /// Mapping from numeric literal expression IDs to their inferred types.
    ///
    /// Populated during inference with type variables; resolved after
    /// substitution to concrete `Numeric` types (`Int`, `Word`, `Float`, etc...).
    /// The interpreter uses this to convert numeric literals to the
    /// correct runtime value type.
    pub(super) numeric_types: HashMap<ExprId, TyId>,
    /// Mapping from conversion expression IDs to their target types.
    ///
    /// Populated when `Into::into` or `TryInto::try_into` methods are called.
    /// The interpreter uses this to dispatch the correct conversion.
    pub(super) convert_targets: HashMap<ExprId, TyId>,
    /// Mapping from wrap expression IDs to their target `Wrappable` types.
    ///
    /// Populated during inference for `?` (wrap) operators; resolved after
    /// substitution to concrete `Option[T]`, `Result[T, E]`, or other
    /// monadic types.
    ///
    /// The interpreter uses this to produce the correct wrapper type.
    pub(super) wrap_types: HashMap<ExprId, TyId>,
    /// Mapping from class method call expression IDs to their receiver's `TypeId`.
    ///
    /// Populated when a class method is called on a newtype or union type.
    /// The interpreter uses this to dispatch to user-defined class instances,
    /// since these types don't carry their `TypeId` in the runtime value
    /// (unlike `type`/sum types which use `Value::Tagged`).
    pub(super) instance_calls: HashMap<ExprId, TypeId>,
    /// Resolved function names for parameterized user class method calls.
    ///
    /// When a parameterized class has multiple instances for the same type
    /// (e.g., `MyInto[A] for X` and `MyInto[B] for X`), the generic
    /// `(ClassId, TypeId, method)` lookup is ambiguous. This map records
    /// the specific generated function name for each call site.
    pub(super) resolved_instance_fns: HashMap<ExprId, StringId>,
}

impl InterpreterOutput {
    fn new() -> Self {
        Self {
            regex_cache: Vec::new(),
            regex_indices: HashMap::new(),
            mempty_types: HashMap::new(),
            numeric_types: HashMap::new(),
            convert_targets: HashMap::new(),
            wrap_types: HashMap::new(),
            instance_calls: HashMap::new(),
            resolved_instance_fns: HashMap::new(),
        }
    }

    /// Resolve all type-variable-bearing maps through the union-find.
    fn resolve(&mut self, uf: &mut UnionFind, arena: &mut TyArena) {
        resolve_map(&mut self.mempty_types, uf, arena);
        resolve_map(&mut self.numeric_types, uf, arena);
        resolve_map(&mut self.convert_targets, uf, arena);
        resolve_map(&mut self.wrap_types, uf, arena);
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
    /// Top-level non-closure `let` bindings hoisted with a provisional type var.
    ///
    /// Pass 1 inserts a fresh var here for each simple `let name = ...` at
    /// script top level (non-interactive only) whose RHS is NOT a closure
    /// literal. Pass 2 `r#let` removes the entry, unifies the provisional
    /// var with the inferred RHS type, and rebinds the name with the final
    /// scheme.
    ///
    /// Closure-RHS `let`s are NOT tracked here; they are hoisted via
    /// `hoist_fun` and use the existing `closure_schemes` rebind path.
    pub(super) lets: HashMap<StringId, TyId>,
    /// Module-level `let` member bindings hoisted with a provisional type var.
    ///
    /// Same purpose as `lets`, scoped per module path so distinct
    /// modules cannot collide. Phase 2 hoists into this map; the
    /// corresponding unify step happens inside `user_module` Pass 2 just
    /// before re-registering the member.
    pub(super) module_lets: HashMap<(QualifiedName, StringId), TyId>,
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
    pub(super) forward_instantiations: HashMap<StmtId, Vec<(TyId, Span)>>,
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

impl HoistState {
    fn new() -> Self {
        Self {
            lets: HashMap::new(),
            module_lets: HashMap::new(),
            funs: HashMap::new(),
            fun_index: HashMap::new(),
            forward_instantiations: HashMap::new(),
            finalized_funs: Vec::new(),
            replay_var_maps: Vec::new(),
        }
    }

    /// Temporarily establish union-find connectivity from `Unify` and
    /// `Callable` constraints. Caller must create and rollback the UF
    /// snapshot.
    ///
    /// Handles:
    /// - `Unify(Var, Var)`: direct union
    /// - `Unify(Fn, Fn)`: sub-unify param and return vars
    /// - `Callable` with `Fn` callee: union param/ret vars
    /// - `Callable` with `Var` callee: union callee with arg/ret vars
    ///
    /// Temporarily takes ownership of `cx.constraints` to avoid
    /// cloning; callers pass a range selecting which constraints to
    /// process.
    fn build_constraint_unions(cx: &mut HoistCtx<'_>, range: Range<usize>) {
        let cs = mem::take(cx.constraints);
        cs[range].iter().for_each(|c| match c {
            Constraint::Unify(a, b, _) => {
                if let (Ty::Var(va), Ty::Var(vb)) =
                    (cx.ty_arena.get(*a), cx.ty_arena.get(*b))
                {
                    let ra = cx.uf.find(*va);
                    let rb = cx.uf.find(*vb);
                    cx.uf.union(ra, rb);
                }
                let ta = cx.ty_arena.get(*a).clone();
                let tb = cx.ty_arena.get(*b).clone();
                if let (Ty::Fn(pa, ra), Ty::Fn(pb, rb)) = (ta, tb) {
                    pa.iter().zip(pb.iter()).for_each(|(&x, &y)| {
                        if let (Ty::Var(vx), Ty::Var(vy)) =
                            (cx.ty_arena.get(x), cx.ty_arena.get(y))
                        {
                            let rx = cx.uf.find(*vx);
                            let ry = cx.uf.find(*vy);
                            cx.uf.union(rx, ry);
                        }
                    });
                    if let (Ty::Var(vr), Ty::Var(vs)) =
                        (cx.ty_arena.get(ra), cx.ty_arena.get(rb))
                    {
                        let rr = cx.uf.find(*vr);
                        let rs = cx.uf.find(*vs);
                        cx.uf.union(rr, rs);
                    }
                }
            }
            Constraint::Callable {
                callee, args, ret, ..
            } => {
                let ct = cx.ty_arena.get(*callee).clone();
                match ct {
                    Ty::Fn(params, fn_ret) => {
                        params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                            if let (Ty::Var(vp), Ty::Var(va)) =
                                (cx.ty_arena.get(p), cx.ty_arena.get(a))
                            {
                                let rp = cx.uf.find(*vp);
                                let ra = cx.uf.find(*va);
                                cx.uf.union(rp, ra);
                            }
                        });
                        if let (Ty::Var(vr), Ty::Var(va)) =
                            (cx.ty_arena.get(fn_ret), cx.ty_arena.get(*ret))
                        {
                            let rr = cx.uf.find(*vr);
                            let ra = cx.uf.find(*va);
                            cx.uf.union(rr, ra);
                        }
                    }
                    Ty::Var(vc) => {
                        if let Ty::Var(vr) = cx.ty_arena.get(*ret) {
                            let rc = cx.uf.find(vc);
                            let rr = cx.uf.find(*vr);
                            cx.uf.union(rc, rr);
                        }
                        args.iter().for_each(|&a| {
                            if let Ty::Var(va) = cx.ty_arena.get(a) {
                                let rc = cx.uf.find(vc);
                                let ra = cx.uf.find(*va);
                                cx.uf.union(rc, ra);
                            }
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        });
        *cx.constraints = cs;
    }

    /// Harvest body-emitted `Class` constraints that are transitively linked
    /// to quantifying vars (via `Unify`/`Callable` constraints) and add them
    /// to the scheme's constraint set.
    ///
    /// Uses UF snapshot/rollback to temporarily process body unifications
    /// without side-effecting the main UF state.
    pub(super) fn harvest_body_class_constraints(
        &self,
        cx: &mut HoistCtx<'_>,
        body_constraint_start: usize,
        vars: &[TyVar],
        scheme_constraints: &mut SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
        declared_tvs: &HashSet<TyVar>,
        tv_names: &HashMap<TyVar, StringId>,
    ) {
        let end = cx.constraints.len();
        let snap = cx.uf.snapshot();

        Self::build_constraint_unions(cx, body_constraint_start..end);

        // Map UF roots to originating quantifying vars; use the first
        // mapping and skip collisions (two distinct type params sharing
        // a root would indicate a unification that should not happen in
        // well-typed code, but we guard defensively)
        let mut root_to_orig: HashMap<TyVar, TyVar> =
            HashMap::with_capacity(vars.len());
        vars.iter().for_each(|&v| {
            root_to_orig.entry(cx.uf.find(v)).or_insert(v);
        });

        // Temporarily take constraints to scan body-emitted class
        // constraints while still having `&mut cx` for UF lookups.
        let cs = mem::take(cx.constraints);
        let harvested = cs[body_constraint_start..end]
            .iter()
            .filter_map(|c| match c {
                Constraint::Class { ty, class, span } => {
                    match cx.ty_arena.get(*ty) {
                        Ty::Var(tv) => {
                            let root = cx.uf.find(*tv);
                            root_to_orig
                                .get(&root)
                                .map(|orig| (*orig, class.clone(), *span))
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .collect::<SmallVec<[(TyVar, TypeClass<TyId>, Span); 2]>>();
        *cx.constraints = cs;

        let mut rejected: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
            SmallVec::new();
        harvested.into_iter().for_each(|(orig, class, span)| {
            let entry = (orig, class.clone());
            if !scheme_constraints.contains(&entry)
                && !rejected.contains(&entry)
            {
                if declared_tvs.contains(&orig) {
                    let param = tv_names
                        .get(&orig)
                        .map(|&n| cx.env.resolve_string(n))
                        .unwrap_or_else(|| "?".to_owned());
                    cx.errors.push(TypeError::MissingTypeParamConstraint {
                        param,
                        class,
                        span,
                    });
                    rejected.push(entry);
                } else {
                    scheme_constraints.push(entry);
                }
            }
        });
        cx.uf.rollback(snap);
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
    ) {
        if let Some(&stmt_id) = self.fun_index.get(&scheme.ty) {
            let is_match = self.funs.get(&stmt_id).is_some_and(|s| s == scheme);
            if is_match {
                self.forward_instantiations
                    .entry(stmt_id)
                    .or_default()
                    .push((inst_ty, span));
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

                insts.into_iter().for_each(|(forward_ty, span)| {
                    let (ty_inst, constraints, var_map) =
                        final_scheme.instantiate_tracked(cx.uf, cx.ty_arena);
                    // Only emit constraints beyond those already emitted
                    // by the original Pass 1 instantiation.
                    constraints.into_iter().skip(pass1_n).for_each(
                        |(ty, class)| {
                            cx.constraints.push(Constraint::Class {
                                ty,
                                class,
                                span,
                            });
                        },
                    );
                    if !var_map.is_empty() {
                        self.replay_var_maps.push(var_map);
                    }
                    cx.constraints
                        .push(Constraint::Unify(forward_ty, ty_inst, span));
                });

                // Check whether the replay introduced class constraints
                // that (through deferred Unify/Callable chains) reach a
                // previously-finalized function's quantified vars. Use
                // the same snapshot/rollback technique as
                // `harvest_body_class_constraints`: temporarily union
                // all deferred constraints, check, then rollback.
                let has_new_class = cx.constraints[constraint_start..]
                    .iter()
                    .any(|c| matches!(c, Constraint::Class { .. }));

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

        Self::build_constraint_unions(cx, 0..end);

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
        cs[constraint_start..end].iter().for_each(|c| {
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
    pub(super) constraints: &'a mut Vec<Constraint>,
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
    /// Examples:
    /// - `a + b` generates `Class { ty: typeof(a), class: Simple(Numeric), span }`
    /// - `opt!` generates `Class { ty: typeof(opt), class: Hkt(Fallible, ?inner), span }`
    /// - `arr[i]` generates `Class { ty: typeof(arr), class: Parameterized(Indexable, ?elem), span }`
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
    /// Arena of type expressions (for converting `TypeExprId -> Ty`).
    pub(super) type_exprs: &'a TypeExprArena,
    /// Runtime environment; used to look up module function type schemes.
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
    constraints: Vec<Constraint>,
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
    /// variables are recorded here. After `resolve_all_types`, we resolve the types
    /// and populate `instance_calls` for any user instances found.
    deferred_instance_calls: Vec<(ExprId, TyId, ClassId)>,
    /// Deferred parameterized user class method calls.
    ///
    /// `(ExprId, ClassId, method, receiver_ty, class_arg_ty)`. After constraint
    /// solving, the class_arg_ty resolves to a concrete type; we look up the
    /// matching instance and record its function name in `resolved_instance_fns`.
    deferred_param_calls: Vec<(ExprId, ClassId, StringId, TyId, TyId)>,
    /// Type variables created for integer literals, for defaulting to `Int`.
    ///
    /// Integer literals are polymorphic (no constraint) so they can unify with
    /// any type (including `Json` in heterogeneous arrays). After constraint
    /// solving, any unresolved type variables in this set default to `Int`.
    numeric_vars: Vec<TyVar>,
    /// Type schemes for polymorphic closures.
    ///
    /// When a closure with type parameters is inferred, its full scheme
    /// (quantified vars + constraints) is stored here. On `let` binding,
    /// we retrieve this scheme for proper generalization instead of
    /// treating the closure as monomorphic.
    pub(super) closure_schemes: HashMap<ExprId, Scheme>,
    /// Declared type-param name mappings for polymorphic closures, consumed
    /// during `let`-binding to pass through to `finalize_hoisted_fun`.
    closure_tv_names: HashMap<ExprId, HashMap<TyVar, StringId>>,
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
    /// Recorded `let` annotations for post-solve union narrowing validation.
    ///
    /// Each entry is `(rhs_ty, ann_ty, span)`. After constraint solving resolves
    /// type variables, these are checked: if `rhs_ty` resolved to a union and
    /// `ann_ty` did not, the annotation illegally narrows a union type.
    let_annotations: Vec<(TyId, TyId, Span)>,
    /// Hoisting and forward-reference tracking state.
    pub(super) hoist: HoistState,
}

impl<'a> InferCtx<'a> {
    /// Create a new inference context.
    ///
    /// The `strings` interner should be shared with the `TypeRegistry` so
    /// type name lookups produce consistent `StringId`s. The `runtime_env`
    /// is used to look up module function type schemes. The `type_exprs`
    /// arena is used to convert `TypeExprId` to `Ty` for user-defined unions.
    /// Set `interactive` to `true` to allow top-level expressions without
    /// requiring a `main` function.
    pub(crate) fn new(
        ast: &'a mut ast::Ast,
        registry: &'a TypeRegistry,
        type_exprs: &'a TypeExprArena,
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
            type_exprs,
            runtime_env,
            env,
            instance_registry: InstanceRegistry::new(),
            ty_arena,
            constraints: Vec::new(),
            uf: UnionFind::new(),
            expr_types: HashMap::new(),
            errors: Vec::new(),
            interp: InterpreterOutput::new(),
            deferred_instance_calls: Vec::new(),
            deferred_param_calls: Vec::new(),
            numeric_vars: Vec::new(),
            closure_schemes: HashMap::new(),
            closure_tv_names: HashMap::new(),
            in_transaction: None,
            next_txn_id: 0,
            class_context: None,
            current_module: None,
            interactive,
            poly_param_vars: HashSet::new(),
            let_annotations: Vec::new(),
            hoist: HoistState::new(),
        }
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
        self.constraints.push(c);
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
    pub(crate) fn constraints(&self) -> &[Constraint] {
        &self.constraints
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
        inst: &super::instance::Instance,
        type_args: &[TyId],
        span: Span,
    ) -> super::ty::Rename {
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
        super::ty::Rename(vars.into_iter().collect())
    }

    /// Create a `SolveCtx` and run constraint solving.
    ///
    /// Takes ownership of constraints, then delegates to
    /// `SolveCtx::solve_constraints`.
    fn solve(&mut self) {
        let constraints = mem::take(&mut self.constraints);
        super::unify::SolveCtx {
            ty_arena: &mut self.ty_arena,
            uf: &mut self.uf,
            registry: self.registry,
            instance_registry: &self.instance_registry,
            env: &self.env,
            errors: &mut self.errors,
            ast: self.ast,
            type_exprs: self.type_exprs,
            current_module: &self.current_module,
            class_context: &self.class_context,
        }
        .solve_constraints(constraints, &self.numeric_vars);
    }

    /// Resolve all inferred types through the union-find.
    ///
    /// Called after constraint solving to replace type variables with their
    /// resolved concrete types.
    pub(crate) fn resolve_all_types(&mut self) {
        resolve_map(&mut self.expr_types, &mut self.uf, &mut self.ty_arena);
        self.interp.resolve(&mut self.uf, &mut self.ty_arena);
    }

    /// Resolve deferred instance calls after constraint solving.
    ///
    /// After constraint solving and type resolution, type variables are resolved
    /// to concrete types. This method iterates through deferred instance call
    /// candidates, resolves their types, and populates `instance_calls` for
    /// any that have user-defined instances.
    pub(crate) fn resolve_deferred_instance_calls(&mut self) {
        // Take ownership to avoid borrow issues
        let deferred = mem::take(&mut self.deferred_instance_calls);

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
                _ => None,
            };

            if let Some(tid) = type_id {
                if self.instance_registry.lookup(kind, tid).is_some() {
                    self.interp.instance_calls.insert(expr_id, tid);
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
                                self.interp
                                    .resolved_instance_fns
                                    .insert(eid, fn_id);
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
            |(rhs, ann, span)| {
                let rhs = self.uf.resolve(rhs, &mut self.ty_arena);
                let ann = self.uf.resolve(ann, &mut self.ty_arena);
                if matches!(self.ty_arena.get(rhs), Ty::Union(..))
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
            },
        );
    }

    /// After constraint solving and substitution application, any remaining
    /// `Ty::Var` or `Ty::Unknown` indicates incomplete inference. This emits
    /// `MissingAnnotation` errors for such cases.
    pub(crate) fn check_remaining_unknowns(&mut self) {
        self.expr_types
            .iter()
            .filter(|(_, &ty)| Self::has_unresolved_vars(ty, &self.ty_arena))
            .map(|(id, _)| {
                self.ast.expr_span(*id).unwrap_or_else(|| Span::new(0, 0))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .for_each(|span| {
                self.errors.push(TypeError::MissingAnnotation(span));
            });

        // Mempty expressions require concrete monoid types; any remaining
        // `Ty::Var` or `Ty::Unknown` means we cannot produce the empty value.
        self.interp
            .mempty_types
            .iter()
            .filter(|(_, &ty)| {
                matches!(self.ty_arena.get(ty), Ty::Var(_) | Ty::Unknown)
            })
            .map(|(id, _)| {
                self.ast.expr_span(*id).unwrap_or_else(|| Span::new(0, 0))
            })
            .collect::<Vec<_>>()
            .into_iter()
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
        // Pass 1: Hoist function and module declarations for forward references
        self.hoist_declarations(stmts);

        // Pass 1.5: Hoist top-level `let` bindings (non-interactive only)
        if !self.interactive {
            self.hoist_toplevel_lets(stmts);
        }

        // Pass 2: Infer types for all statement bodies
        stmts.iter().for_each(|id| self.stmt(*id));

        // Solve collected constraints (updates union-find in-place)
        self.solve();

        // Enable zonk cache for resolution passes (bindings are frozen
        // post-solve, so memoization is safe and avoids redundant tree walks)
        self.uf.enable_zonk_cache();

        // Resolve all type variables through the union-find
        self.resolve_all_types();

        // Resolve deferred instance calls (now that types are resolved)
        self.resolve_deferred_instance_calls();

        // Resolve parameterized user class method call function names
        self.resolve_deferred_param_calls();

        self.uf.disable_zonk_cache();

        // Check for illegal union narrowing via let annotations
        self.check_let_union_narrowing();

        // Check for remaining unresolved type variables
        self.check_remaining_unknowns();

        // Validate main entry point (in non-interactive mode)
        self.validate_main_entry_point(stmts);

        self.into_output(registry, arena)
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
            Ok(TypecheckOutput {
                ty_arena: self.ty_arena,
                regex_cache: self.interp.regex_cache,
                regex_indices: self.interp.regex_indices,
                mempty_types: self.interp.mempty_types,
                numeric_types: self.interp.numeric_types,
                convert_targets: self.interp.convert_targets,
                wrap_types: self.interp.wrap_types,
                instance_calls: self.interp.instance_calls,
                resolved_instance_fns: self.interp.resolved_instance_fns,
                class_registry: self.env.class_registry,
            })
        }
    }
}
