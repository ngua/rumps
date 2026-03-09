//! User-defined typeclass instance registry.
//!
//! Tracks which user types implement which builtin classes. Used during
//! constraint solving to determine if a `Ty::Named` satisfies a class
//! constraint via a user-provided implementation.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use smallvec::SmallVec;

use super::ty::{BuiltinClass, BuiltinClassTag, TyId, TyVar};
use super::TypeError;
use crate::intern::StringId;
use crate::{Span, TypeId};

/// An associated type definition within a class instance.
///
/// Example: `NEWTYPE Index = Int` inside `CLASS Indexable[T] FOR MyVec[T] { ... }`
/// defines `.Index` for `MyVec[T]` to be `Int`.
#[derive(Clone, Debug)]
pub(crate) struct AssocTypeDef {
    /// The associated type name (e.g., `"Index"`).
    pub(crate) name: StringId,
    /// The concrete type this instance defines for the associated type.
    pub(crate) ty: TyId,
    /// Optional constraints on the associated type (e.g., `: Ord`).
    pub(crate) constraints: SmallVec<[BuiltinClass<TyId>; 1]>,
    /// Source span for error messages.
    pub(crate) span: Span,
}

/// A user-defined instance of a builtin class for a user type.
///
/// Example: `CLASS Display FOR Point { ... }` creates an instance with
/// `class = Display`, `for_type = Point's TypeId`.
///
/// For parameterized instances like `CLASS Display FOR Either[L, R] WHERE L: Display`,
/// `type_params` holds `[L, R]` and `constraints` holds `[(L, Display)]`.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// The class being implemented (e.g., `BuiltinClassTag::Display`).
    pub(crate) class: BuiltinClassTag,
    /// Type arguments to the class (e.g., `[TyArena::STRING]` for `Into[String]`).
    pub(crate) class_args: SmallVec<[TyId; 2]>,
    /// Type parameters on the implementing type (e.g., `[L, R]` for `Either[L, R]`).
    pub(crate) type_params: SmallVec<[TyVar; 2]>,
    /// WHERE clause constraints (e.g., `[(L, Display), (R, Display)]`).
    pub(crate) constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]>,
    /// Method implementations: method name -> generated function name.
    ///
    /// Populated in Phase 5 (Resolution) when `CLASS` statements are lowered.
    /// The generated function name follows the pattern `__inst_{Class}_{Type}_{method}`.
    pub(crate) methods: HashMap<StringId, StringId>,
    /// Associated type definitions for this instance (e.g., `NEWTYPE Index = Int`).
    ///
    /// Currently empty; will be populated when associated types are parsed (Phase 2).
    pub(crate) assoc_types: SmallVec<[AssocTypeDef; 1]>,
    /// Owning module path, or `None` for top-level instances.
    pub(crate) module: Option<StringId>,
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
/// Keyed by `(BuiltinClassTag, TypeId)` for O(1) lookup during constraint solving.
/// A type can have at most one instance per class.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstanceRegistry {
    instances: HashMap<(BuiltinClassTag, TypeId), Instance>,
}

impl InstanceRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Look up an instance for a class and type.
    pub(crate) fn lookup(
        &self,
        class: BuiltinClassTag,
        type_id: TypeId,
    ) -> Option<&Instance> {
        self.instances.get(&(class, type_id))
    }

    /// Register a new instance.
    ///
    /// Returns `Err` if an instance already exists for this `(class, type_id)`.
    pub(crate) fn register(
        &mut self,
        type_id: TypeId,
        inst: Instance,
    ) -> Result<(), TypeError> {
        match self.instances.entry((inst.class, type_id)) {
            Entry::Occupied(_) => Err(TypeError::DuplicateInstance {
                class: inst.class,
                type_id,
                span: inst.span,
            }),
            Entry::Vacant(e) => {
                e.insert(inst);
                Ok(())
            }
        }
    }
}
