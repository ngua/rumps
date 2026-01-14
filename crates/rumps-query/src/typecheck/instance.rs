//! User-defined typeclass instance registry.
//!
//! Tracks which user types implement which builtin classes. Used during
//! constraint solving to determine if a `Ty::Named` satisfies a class
//! constraint via a user-provided implementation.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use smallvec::SmallVec;

use super::ty::{Class, ClassKind, Ty, TyVar};
use super::TypeError;
use crate::intern::StringId;
use crate::{Span, TypeId};

/// Identifier for a generated instance method function.
///
/// TODO: Wire this up in Phase 5 (Resolution) when instance methods are
/// lowered to named functions. For now this is a placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FnId(pub(crate) u32);

/// A user-defined instance of a builtin class for a user type.
///
/// Example: `CLASS Display FOR Point { ... }` creates an instance with
/// `class = Display`, `for_type = Point's TypeId`.
///
/// For parameterized instances like `CLASS Display FOR Either[L, R] WHERE L: Display`,
/// `type_params` holds `[L, R]` and `constraints` holds `[(L, Display)]`.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    /// The class being implemented (e.g., `ClassKind::Display`).
    pub(crate) class: ClassKind,
    /// Type arguments to the class (e.g., `[Ty::String]` for `Into[String]`).
    pub(crate) class_args: SmallVec<[Ty; 2]>,
    /// Type parameters on the implementing type (e.g., `[L, R]` for `Either[L, R]`).
    pub(crate) type_params: SmallVec<[TyVar; 2]>,
    /// WHERE clause constraints (e.g., `[(L, Display), (R, Display)]`).
    pub(crate) constraints: SmallVec<[(TyVar, Class); 2]>,
    /// Method implementations: method name -> generated function ID.
    ///
    /// Populated in Phase 5 (Resolution) when `CLASS` statements are lowered.
    pub(crate) methods: HashMap<StringId, FnId>,
    /// Source span for error messages.
    pub(crate) span: Span,
}

/// Registry of user-defined class instances.
///
/// Keyed by `(ClassKind, TypeId)` for O(1) lookup during constraint solving.
/// A type can have at most one instance per class.
#[derive(Clone, Debug, Default)]
pub(crate) struct InstanceRegistry {
    instances: HashMap<(ClassKind, TypeId), Instance>,
}

impl InstanceRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Look up an instance for a class and type.
    pub(crate) fn lookup(
        &self,
        class: ClassKind,
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
