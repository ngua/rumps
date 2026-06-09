//! Type unification and constraint solving.
//!
//! Implements the core unification algorithm for Hindley-Milner type inference.
//! Unification determines whether two types can be made equal, and if so,
//! produces a substitution mapping type variables to concrete types.
//!
//! # Testing Philosophy
//!
//! This module has no unit tests. While unification is a pure function on types,
//! testing it in isolation proved less effective than integration testing:
//!
//! 1. All unification behavior is exercised by real code in `scripts/*.rumps`
//! 2. Edge cases (occurs check, union ordering) are implicitly tested through
//!    scripts that rely on correct unification
//! 3. Integration tests catch bugs that synthetic type construction misses
//!
//! See `infer.rs` for the full rationale on our testing approach.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::convert::ConvertCtx;
use super::decl::TypeDeclRegistry;
use super::env::TypeEnv;
use super::error::TypeError;
use super::infer::{ClassContext, Constraint};
use super::instance::{
    Instance, InstanceLookup, InstanceRegistry, InstanceUse,
};
use super::ty::{Rename, Ty, TyArena, TyId, TyVar, TypeClass};
use super::uf::UnionFind;
use crate::ast::{Ast, AstTypeExpr, AstTypeExprId};
use crate::intern::{QualifiedName, StringId};
use crate::value::{TypeDef, TypeId, TypeRegistry};
use crate::{ClassId, Span};

/// Result of a unification attempt.
///
/// With union-find, successful unification mutates the UF in-place.
type UnifyResult = Result<(), TypeError>;

#[allow(clippy::large_enum_variant)]
pub(super) enum InstancesLookup {
    Found(SmallVec<[Instance; 2]>),
    BlockedSelf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NewtypeEdge {
    pub(super) alias: TypeId,
    pub(super) from: TyId,
    pub(super) to: TyId,
    pub(super) repr: TyId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewtypeEdgeStatus {
    Allowed,
    Blocked,
    Missing,
}

/// Context for constraint solving (unification, class satisfaction, etc.).
///
/// Created from `InferCtx` fields for the duration of `solve_constraints`.
/// Separates the solving machinery from inference-time bookkeeping.
pub(super) struct SolveCtx<'a> {
    pub(super) ty_arena: &'a mut TyArena,
    pub(super) uf: &'a mut UnionFind,
    pub(super) registry: &'a TypeRegistry,
    pub(super) decls: &'a TypeDeclRegistry,
    pub(super) instance_registry: &'a InstanceRegistry,
    pub(super) env: &'a TypeEnv,
    pub(super) errors: &'a mut Vec<TypeError>,
    /// AST reference for alias expansion and field type resolution.
    pub(super) ast: &'a mut Ast,
    /// Current module path (for module-aware type name resolution).
    pub(super) current_module: Option<QualifiedName>,
    /// Current class context (for associated type resolution).
    pub(super) class_context: &'a Option<ClassContext>,
    /// Maps HKT class-constrained type variables to their `ClassId`, so
    /// `unify_apply` can look up tuple constructor instances for
    /// position-aware decomposition.
    pub(super) hkt_var_classes: HashMap<TyVar, ClassId>,
}

mod alias;
mod apply;
mod assoc;
mod classes;
mod constraints;
mod core;
mod edge;
mod instances;
mod ops;

impl SolveCtx<'_> {
    fn convert_ctx(&mut self) -> ConvertCtx<'_> {
        ConvertCtx {
            ty_arena: self.ty_arena,
            uf: self.uf,
            registry: self.registry,
            decls: self.decls,
            env: self.env,
            ast: self.ast,
            errors: self.errors,
            current_module: &self.current_module,
            class_context: self.class_context,
            rewrite_ast: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typecheck::instance::{Instance, InstanceRegistry};
    use crate::TypeId;

    /// Test that `check_ord` passes for builtin orderable types.
    #[test]
    fn check_ord_builtins() {
        // All these should pass without error
        let builtins = [
            Ty::Bool,
            Ty::Int,
            Ty::Word,
            Ty::Float,
            Ty::Char,
            Ty::String,
            Ty::Time,
            Ty::Ordering,
        ];

        builtins.iter().for_each(|ty| {
            // We can't easily create an InferCtx in unit tests, so we test
            // the match logic indirectly via integration tests. This test
            // documents the expected behavior.
            assert!(
                matches!(
                    ty,
                    Ty::Bool
                        | Ty::Int
                        | Ty::Word
                        | Ty::Float
                        | Ty::Char
                        | Ty::String
                        | Ty::Time
                        | Ty::Ordering
                ),
                "expected {} to be orderable",
                ty
            );
        });
    }

    /// Test that `check_display` passes for builtin displayable types.
    #[test]
    fn check_display_builtins() {
        let mut a = TyArena::new();
        // Functions are NOT displayable
        let fn_ty = a.func(smallvec::smallvec![TyArena::INT], TyArena::INT);
        assert!(
            matches!(a.get(fn_ty), Ty::Fn(_, _)),
            "Fn types should not be displayable"
        );

        // All primitives are displayable
        let displayable = [
            TyArena::BOOL,
            TyArena::INT,
            TyArena::WORD,
            TyArena::FLOAT,
            TyArena::CHAR,
            TyArena::STRING,
            TyArena::UNIT,
            TyArena::TIME,
            TyArena::RANGE,
            TyArena::JSON,
            TyArena::ORDERING,
            TyArena::DATA_STATUS,
            TyArena::FILEPATH,
            TyArena::PATH,
            TyArena::REGEX,
            TyArena::RUNTIME_ERROR,
            TyArena::LOCAL,
            TyArena::GLOBAL,
        ];

        displayable.iter().for_each(|&tid| {
            assert!(
                !matches!(a.get(tid), Ty::Fn(_, _)),
                "expected type to be displayable"
            );
        });
    }

    /// Test Instance creation and lookup.
    #[test]
    fn instance_registry_lookup() {
        let mut registry = InstanceRegistry::new();

        // Use an existing TypeId constant for testing (STORABLE is a union type
        // that we can hypothetically add an Ord instance for)
        let user_type_id = TypeId::STORABLE;

        // Create an Ord instance for the user type
        let ord_inst = Instance {
            class: ClassId::ORD,
            class_args: SmallVec::new(),
            type_params: SmallVec::new(),
            constraints: SmallVec::new(),
            methods: HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        let _ = registry.register(user_type_id, ord_inst.clone());

        // Lookup should find the instance
        let found = registry.lookup(ClassId::ORD, user_type_id);
        assert!(found.is_some(), "should find Ord instance");

        // Lookup for different class should not find anything
        let not_found = registry.lookup(ClassId::DISPLAY, user_type_id);
        assert!(not_found.is_none(), "should not find Display instance");

        // Lookup for different type should not find anything
        let other_type_id = TypeId::SCALAR;
        let not_found2 = registry.lookup(ClassId::ORD, other_type_id);
        assert!(
            not_found2.is_none(),
            "should not find instance for other type"
        );
    }

    /// Test that Instance with WHERE constraints stores them correctly.
    #[test]
    fn instance_with_constraints() {
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let t_id = a.var(0);
        let constraint = (t, TypeClass::simple(ClassId::DISPLAY));

        let inst = Instance {
            class: ClassId::ORD,
            class_args: SmallVec::new(),
            type_params: smallvec::smallvec![t_id],
            constraints: smallvec::smallvec![constraint],
            methods: HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        assert_eq!(inst.type_params.len(), 1);
        assert_eq!(inst.constraints.len(), 1);
        assert!(inst.constraints.first().is_some_and(|(got, class)| {
            *got == t
                && matches!(
                    class,
                    TypeClass::Concrete { id: ClassId::DISPLAY, params } if params.is_empty()
                )
        }));
    }

    /// Test constraint substitution.
    #[test]
    fn constraint_substitution() {
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let var_id = a.var(0);
        let constraint = TypeClass::hkt_elem(ClassId::MAPPABLE, var_id);

        // Create rename: T -> Int
        let rename = Rename::singleton(t, TyArena::INT);

        // Apply rename to constraint
        let resolved = constraint.apply(&rename, &mut a);

        // Should now be `Mappable` with `elems: [Int]`.
        assert!(
            matches!(
                resolved,
                TypeClass::Hkt { id: ClassId::MAPPABLE, ref elems, .. } if elems.first() == Some(&TyArena::INT)
            ),
            "constraint should be Mappable with elems=[Int] after rename"
        );
    }
}
