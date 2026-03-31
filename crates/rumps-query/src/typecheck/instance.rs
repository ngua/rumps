//! User-defined typeclass instance registry.
//!
//! Tracks which user types implement which builtin classes. Used during
//! constraint solving to determine if a `Ty::Named` satisfies a class
//! constraint via a user-provided implementation.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use smallvec::SmallVec;

use super::ty::{TyId, TyVar, TypeClass};
use super::TypeError;
use crate::intern::{QualifiedName, StringId};
use crate::{ClassId, Span, TypeId};

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

/// A user-defined instance of a builtin class for a user type.
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
    /// Associated type definitions for this instance (e.g., `newtype Index = Int`).
    ///
    /// Currently empty; will be populated when associated types are parsed (Phase 2).
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
/// Keyed by `(ClassId, TypeId)` for O(1) lookup during constraint solving.
/// A type can have at most one instance per class.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstanceRegistry {
    instances: HashMap<(ClassId, TypeId), Instance>,
}

impl InstanceRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Look up an instance for a class and type.
    pub(crate) fn lookup(
        &self,
        class: ClassId,
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
