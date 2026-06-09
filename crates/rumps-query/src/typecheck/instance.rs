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

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub(crate) enum InstanceLookup {
    Found(Instance),
    Missing,
    BlockedSelf,
    NotImported,
}

/// An associated type definition within a class instance.
///
/// Example: `newtype Index = Int` inside `class Indexable[T] FOR MyVec[T] { ... }`
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
/// Example: `class Display FOR Point { ... }` creates an instance with
/// `class = Display`, `for_type = Point's TypeId`.
///
/// For parameterized instances like `class Display FOR Either[L, R] WHERE L: Display`,
/// `type_params` holds `[L, R]` and `constraints` holds `[(L, Display)]`.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// The class being implemented (e.g., `ClassId::Display`).
    pub(crate) class: ClassId,
    /// Type arguments to the class (e.g., `[TyArena::STRING]` for `Into[String]`).
    pub(crate) class_args: SmallVec<[TyId; 2]>,
    /// All type arguments on the implementing type in positional order.
    ///
    /// Contains both `Ty::Var` entries (polymorphic params) and concrete
    /// types (fixed params like `Int` in `class Fallible for Pair[Int]`).
    /// Zipped 1:1 with the actual `type_args` at use sites.
    pub(crate) type_params: SmallVec<[TyId; 2]>,
    /// WHERE clause constraints (e.g., `[(L, Display), (R, Display)]`).
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
    /// prefer `has_with_args` when checking for a specific class arg combination.
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

    /// Check whether an instance with the given `class_args` already exists.
    pub(crate) fn has_with_args(
        &self,
        class: ClassId,
        type_id: TypeId,
        args: &[TyId],
    ) -> bool {
        self.instances
            .get(&(class, type_id))
            .is_some_and(|v| v.iter().any(|i| i.class_args.as_slice() == args))
    }

    /// Register a new instance.
    ///
    /// Returns `Err` if an instance with the same `class_args` already
    /// exists for this `(class, type_id)`.
    pub(crate) fn register(
        &mut self,
        type_id: TypeId,
        inst: Instance,
    ) -> Result<(), TypeError> {
        let v = self.instances.entry((inst.class, type_id)).or_default();
        if v.iter()
            .any(|i| i.class_args.as_slice() == inst.class_args.as_slice())
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
