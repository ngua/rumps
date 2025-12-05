//! Runtime dispatch for user-defined class instances.
//!
//! At runtime, when invoking a class method on a user type, we look up whether
//! there's a user-defined instance and dispatch to the generated function if so.
//!
//! User types can be:
//! - `variant` declarations: variant data with type metadata on `Value`
//! - `newtype` (type aliases): primitive or structural representation values
//! - `union` (union types): member representation values
//!
//! Dispatch uses checked expression metadata first, then value `ty` and `repr`.

use std::collections::HashMap;

use crate::intern::StringId;
use crate::value::TypeId;
use crate::ClassId;

/// Runtime representation of a user-defined class instance.
///
/// Contains the mapping from method names to generated function names.
/// Type parameters and constraints are erased at runtime; the type checker
/// guarantees all uses are valid.
#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeInstance {
    /// Method name -> generated function name (StringId).
    ///
    /// The generated function name follows the pattern `__inst_{Class}_{Type}_{method}`
    /// and is registered in the interpreter's `functions` map.
    pub(crate) methods: HashMap<StringId, StringId>,
}

impl RuntimeInstance {
    /// Generate the internal function name for a class instance method.
    ///
    /// Pattern: `__inst_{Class}_{Type}__{method}` for non-parameterized classes,
    /// or `__inst_{Class}_{Arg1}_{Arg2}_.._{Type}__{method}` for parameterized.
    ///
    /// These names are internal and not user-callable directly. They follow a
    /// consistent format so both the typechecker and interpreter can independently
    /// generate the same names.
    ///
    /// The `type_name` may contain `.` for module-qualified types, e.g. `"Shapes.Circle"`,
    /// which is sanitized to `_` to avoid path-like function names.
    pub(crate) fn fn_name(
        class_name: &str,
        type_name: &str,
        method: &str,
        class_args: &[&str],
    ) -> String {
        // Sanitize `.` to `_` for module-qualified type names.
        let safe_name = type_name.replace('.', "_");
        if class_args.is_empty() {
            format!("__inst_{class_name}_{safe_name}__{method}")
        } else {
            let args = class_args.join("_");
            format!("__inst_{class_name}_{args}_{safe_name}__{method}")
        }
    }

    /// Like `fn_name`, but accepts owned `String` class args.
    ///
    /// Avoids the repeated `Vec<String>` -> `Vec<&str>` conversion at call sites.
    pub(crate) fn fn_name_owned(
        class_name: &str,
        type_name: &str,
        method: &str,
        class_args: &[String],
    ) -> String {
        let refs: Vec<&str> = class_args.iter().map(String::as_str).collect();
        Self::fn_name(class_name, type_name, method, &refs)
    }

    /// Generate the internal function name for a class default method.
    pub(crate) fn default_fn_name(class_name: &str, method: &str) -> String {
        format!("__class_default_{class_name}__{method}")
    }

    /// Look up a method by name, returning the generated function name.
    pub(crate) fn lookup(&self, method: StringId) -> Option<StringId> {
        self.methods.get(&method).copied()
    }
}

/// Registry of user-defined class instances for runtime dispatch.
///
/// Keyed by `(ClassId, TypeId)` for O(1) lookup during method dispatch.
#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeInstanceRegistry {
    instances: HashMap<(ClassId, TypeId), RuntimeInstance>,
    tuple_methods: HashMap<(ClassId, usize, StringId), StringId>,
}

impl RuntimeInstanceRegistry {
    /// Create an empty registry.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Look up an instance for a class and type.
    pub(crate) fn lookup(
        &self,
        class: ClassId,
        type_id: TypeId,
    ) -> Option<&RuntimeInstance> {
        self.instances.get(&(class, type_id))
    }

    /// Look up a specific method for a class and type.
    ///
    /// Returns the generated function name if found.
    pub(crate) fn lookup_method(
        &self,
        class: ClassId,
        type_id: TypeId,
        method: StringId,
    ) -> Option<StringId> {
        self.lookup(class, type_id)
            .and_then(|inst| inst.lookup(method))
    }

    pub(crate) fn lookup_tuple_method(
        &self,
        class: ClassId,
        arity: usize,
        method: StringId,
    ) -> Option<StringId> {
        self.tuple_methods.get(&(class, arity, method)).copied()
    }

    /// Register an instance.
    ///
    /// Overwrites any existing instance for the same `(class, type_id)`.
    /// The typechecker prevents duplicates, so this is safe.
    pub(crate) fn register(
        &mut self,
        class: ClassId,
        type_id: TypeId,
        inst: RuntimeInstance,
    ) {
        self.register_with_tuple_arity(class, type_id, None, inst);
    }

    pub(crate) fn register_with_tuple_arity(
        &mut self,
        class: ClassId,
        type_id: TypeId,
        arity: Option<usize>,
        inst: RuntimeInstance,
    ) {
        arity.into_iter().for_each(|arity| {
            inst.methods.iter().for_each(|(&method, &fun)| {
                self.tuple_methods.insert((class, arity, method), fun);
            });
        });
        self.instances.insert((class, type_id), inst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that two types with the same underlying representation but
    /// different TypeIds dispatch to different instances.
    ///
    /// This simulates the newtype scenario:
    /// ```rumps
    /// newtype X = Int
    /// newtype Y = Int
    /// class Numeric for X { ... }
    /// class Numeric for Y { ... }
    /// ```
    ///
    /// Even though both wrap `Int`, they have distinct `TypeId`s and should
    /// dispatch to their own implementations.
    #[test]
    fn distinct_types_dispatch_independently() {
        use crate::intern::StringInterner;

        let mut interner = StringInterner::new();
        let add = interner.intern("add");
        let fn_x = interner.intern("__inst_Numeric_X_add");
        let fn_y = interner.intern("__inst_Numeric_Y_add");

        // Two distinct TypeIds (simulating newtype X and newtype Y)
        // Use builtin TypeIds as stand-ins for user types
        let type_x = TypeId::BOOL;
        let type_y = TypeId::INT;

        let mut registry = RuntimeInstanceRegistry::new();

        // Register Numeric instance for X
        let mut inst_x = RuntimeInstance::default();
        inst_x.methods.insert(add, fn_x);
        registry.register(ClassId::NUMERIC, type_x, inst_x);

        // Register Numeric instance for Y
        let mut inst_y = RuntimeInstance::default();
        inst_y.methods.insert(add, fn_y);
        registry.register(ClassId::NUMERIC, type_y, inst_y);

        // X dispatches to fn_x
        assert_eq!(
            registry.lookup_method(ClassId::NUMERIC, type_x, add),
            Some(fn_x)
        );

        // Y dispatches to fn_y
        assert_eq!(
            registry.lookup_method(ClassId::NUMERIC, type_y, add),
            Some(fn_y)
        );

        // X and Y return different functions
        assert_ne!(fn_x, fn_y);
    }

    /// Verify that different classes for the same type are independent.
    #[test]
    fn different_classes_same_type() {
        use crate::intern::StringInterner;

        let mut interner = StringInterner::new();
        let display = interner.intern("display");
        let into = interner.intern("into");
        let fn_display = interner.intern("__inst_Display_Point_display");
        let fn_into = interner.intern("__inst_Into_Point_into");

        // Use a builtin TypeId as stand-in for a user type
        let type_point = TypeId::CHAR;

        let mut registry = RuntimeInstanceRegistry::new();

        // Register Display for Point
        let mut inst_display = RuntimeInstance::default();
        inst_display.methods.insert(display, fn_display);
        registry.register(ClassId::DISPLAY, type_point, inst_display);

        // Register Into for Point
        let mut inst_into = RuntimeInstance::default();
        inst_into.methods.insert(into, fn_into);
        registry.register(ClassId::INTO, type_point, inst_into);

        // Display:display dispatches correctly
        assert_eq!(
            registry.lookup_method(ClassId::DISPLAY, type_point, display),
            Some(fn_display)
        );

        // Into:into dispatches correctly
        assert_eq!(
            registry.lookup_method(ClassId::INTO, type_point, into),
            Some(fn_into)
        );

        // Cross-lookup returns None
        assert_eq!(
            registry.lookup_method(ClassId::DISPLAY, type_point, into),
            None
        );
        assert_eq!(
            registry.lookup_method(ClassId::INTO, type_point, display),
            None
        );
    }
}
