//! Typeclass instance registry.
//!
//! Tracks which types implement which classes (both builtin and user-defined).
//! Used during constraint solving to determine if a type satisfies a class
//! constraint via a user-provided implementation.

use std::collections::HashMap;

use smallvec::SmallVec;

use super::ty::{TyId, TyVar, TypeClass};
use super::TypeError;
use crate::intern::{QualifiedName, StringId};
use crate::{ClassId, Span, TypeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstanceUse {
    Evidence,
    ExplicitCall,
    MethodValue,
    Derive,
    Super,
}

/// An associated type definition within a class instance.
///
/// Example: `newtype Index = Int` inside `class Indexable[T] for MyVec[T] { ... }`
/// defines `.Index` for `MyVec[T]` to be `Int`.
#[derive(Clone, Debug)]
pub(crate) struct AssocTypeDef {
    /// The associated type name (e.g., `"Index"`).
    pub(crate) name: StringId,
    /// The concrete type this instance defines for the associated type.
    pub(crate) ty: TyId,
    /// Optional constraints on the associated type (e.g., `: Ord`).
    pub(crate) constraints: SmallVec<[TypeClass<TyId>; 1]>,
    /// Source span for error messages.
    pub(crate) span: Span,
}

/// A user-defined instance of a class for a type.
///
/// Example: `class Display for Point { ... }` creates an instance with
/// `class = Display`, `for_type = Point's TypeId`.
///
/// For parameterized instances like `class Display for Either[L, R] where L: Display`,
/// `type_params` holds `[L, R]` and `constraints` holds `[(L, Display)]`.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// The class being implemented (e.g., `ClassId::Display`).
    pub(crate) class: ClassId,
    /// Fixed class arguments in declaration order.
    ///
    /// These are `TypeClass::Concrete.params` or `TypeClass::Hkt.params`.
    pub(crate) class_args: SmallVec<[TyId; 2]>,
    /// Higher kinded element arguments in declaration order.
    ///
    /// These are only `TypeClass::Hkt.elems`; concrete class uses leave this
    /// empty.
    pub(crate) hkt_elems: SmallVec<[TyId; 2]>,
    /// The complete implementing receiver argument pattern in positional order.
    ///
    /// This contains variables and fixed arguments, and is zipped `1:1` with
    /// the actual receiver arguments at use sites.
    pub(crate) type_params: SmallVec<[TyId; 2]>,
    /// `where` clause constraints (e.g., `[(L, Display), (R, Display)]`).
    pub(crate) constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]>,
    /// Method implementations: method name -> generated function name.
    ///
    /// Populated in Phase 5 (Resolution) when `class` statements are lowered.
    /// The generated function name follows the pattern `__inst_{Class}_{Type}_{method}`.
    pub(crate) methods: HashMap<StringId, StringId>,
    /// Associated type definitions for this instance.
    ///
    /// The RHS may reference instance type parameters. Projection resolution
    /// substitutes those parameters with the saturated use site type args.
    pub(crate) assoc_types: SmallVec<[AssocTypeDef; 1]>,
    /// Owning module path, or `None` for top-level instances.
    pub(crate) module: Option<QualifiedName>,
    /// Source span for error messages.
    pub(crate) span: Span,
}

impl Instance {
    /// Compares the registry identity used before complete pattern matching.
    ///
    /// Concrete uses compare `class_args`; higher kinded uses compare
    /// `hkt_elems`, preserving the old overloaded field behavior. Phase `3B`
    /// must delete this compatibility comparison.
    fn has_legacy_args(&self, class_args: &[TyId], hkt_elems: &[TyId]) -> bool {
        if hkt_elems.is_empty() {
            self.hkt_elems.is_empty()
                && self.class_args.as_slice() == class_args
        } else {
            self.hkt_elems.as_slice() == hkt_elems
        }
    }

    /// Look up an associated type definition by name.
    ///
    /// Returns `None` if this instance does not define the named associated type.
    pub(crate) fn get_assoc_type(
        &self,
        name: StringId,
    ) -> Option<&AssocTypeDef> {
        self.assoc_types.iter().find(|a| a.name == name)
    }
}

/// Registry of user-defined class instances.
///
/// Keyed by `(ClassId, TypeId)` for lookup during constraint solving.
/// For non-parameterized classes, a type has at most one instance per class.
/// For parameterized classes (e.g., `MyInto[T]`), a type may have multiple
/// instances with different class type arguments.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstanceRegistry {
    instances: HashMap<(ClassId, TypeId), SmallVec<[Instance; 1]>>,
}

impl InstanceRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Look up the first (or only) instance for a class and type.
    ///
    /// For non-parameterized classes this always returns the unique instance.
    /// For parameterized classes with multiple instances, returns the first;
    /// `has_with_args` preserves the Phase `2` registration check.
    pub(crate) fn lookup(
        &self,
        class: ClassId,
        type_id: TypeId,
    ) -> Option<&Instance> {
        self.instances
            .get(&(class, type_id))
            .and_then(|v| v.first())
    }

    /// Look up all instances for a class and type.
    ///
    /// Returns an empty slice when no instances exist.
    pub(crate) fn lookup_all(
        &self,
        class: ClassId,
        type_id: TypeId,
    ) -> &[Instance] {
        self.instances
            .get(&(class, type_id))
            .map(SmallVec::as_slice)
            .unwrap_or(&[])
    }

    /// Look up a tuple instance by class and arity.
    ///
    /// Tuple types may have multiple instances for the same class (one per
    /// arity), so this finds the instance whose `type_params` length matches
    /// the given `arity`.
    pub(crate) fn lookup_tuple(
        &self,
        class: ClassId,
        arity: usize,
    ) -> Option<&Instance> {
        self.lookup_all(class, TypeId::TUPLE)
            .iter()
            .find(|i| i.type_params.len() == arity)
    }

    /// Check whether an instance with the Phase `2` registry identity exists.
    pub(crate) fn has_with_args(
        &self,
        class: ClassId,
        type_id: TypeId,
        class_args: &[TyId],
        hkt_elems: &[TyId],
    ) -> bool {
        self.instances.get(&(class, type_id)).is_some_and(|v| {
            v.iter().any(|i| i.has_legacy_args(class_args, hkt_elems))
        })
    }

    /// Register a new instance.
    ///
    /// Returns `Err` if an instance with the same Phase `2` registry identity
    /// exists for this `(class, type_id)`.
    pub(crate) fn register(
        &mut self,
        type_id: TypeId,
        inst: Instance,
    ) -> Result<(), TypeError> {
        let v = self.instances.entry((inst.class, type_id)).or_default();
        if v.iter()
            .any(|i| i.has_legacy_args(&inst.class_args, &inst.hkt_elems))
        {
            Err(TypeError::DuplicateInstance {
                class: inst.class,
                type_id,
                span: inst.span,
            })
        } else {
            v.push(inst);
            Ok(())
        }
    }
}
