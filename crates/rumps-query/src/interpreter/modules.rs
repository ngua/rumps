//! Module function and constant evaluation.
//!
//! Handles `Expr::Path` nodes that refer to module-qualified functions
//! (e.g., `Iter.length`, `String.split`) or constants (e.g., `Math.pi`).
//! Paths are resolved during the parse-time resolution pass and evaluated
//! here at runtime.
//!
//! # Nested Modules
//!
//! The architecture supports nested modules (e.g., `Math.Trig.sin`) for
//! future user-defined modules:
//!
//! ```text
//! Math
//! ├── sqrt
//! ├── abs
//! ├── pi      (constant)
//! ├── e       (constant)
//! └── Trig
//!     ├── sin
//!     └── cos
//! ```
//!
//! Paths of any length are supported; the first segment must be a registered
//! module, and each subsequent segment (except the last) must be a submodule.

use smallvec::SmallVec;

use super::Interpreter;
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::Value;
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a namespace path to a module function.
    ///
    /// Handles paths of any length:
    /// - `Iter.length` -> `Value::ModuleFn { path: ["Array", "length"] }`
    /// - `Math.Trig.sin` -> `Value::ModuleFn { path: ["Math", "Trig", "sin"] }`
    ///
    /// If the path doesn't resolve to a module function, falls back to
    /// treating it as a type variant path (for user-defined types registered
    /// at runtime).
    pub(super) fn path(
        &mut self,
        segments: &[StringId],
        span: Span,
    ) -> Result<Value> {
        // Need at least two segments: module + function (or type + variant)
        // Typechecker validates path structure
        let (&first, _) = segments
            .split_first()
            .unwrap_or_else(|| typechecked!("path", "non-empty"));

        // Check if the first segment is a module
        if self.env.has_module(first) {
            self.module_path(segments)
        } else {
            // Fall back to type + variant interpretation
            self.type_variant_path(segments, span)
        }
    }

    /// Resolve a path as a module function or constant.
    ///
    /// The path must have at least two segments. The last segment is the
    /// function/constant name; all preceding segments form the module path.
    /// Checks both builtin and user-defined modules.
    fn module_path(&mut self, segments: &[StringId]) -> Result<Value> {
        // Check for builtin module function first
        if self.env.module_fn_exists(segments) {
            let path: SmallVec<[StringId; 4]> = segments.into();
            Ok(Value::ModuleFn { path })
        }
        // Check for builtin module constant
        else if let Some(const_id) = self.env.get_module_const(segments) {
            Ok(self
                .env
                .consts
                .get(const_id)
                .cloned()
                .unwrap_or_else(|| invariant!("ConstId in consts map")))
        }
        // Check for user module function
        else if self.env.user_module_fn_exists(segments) {
            // Return ModuleFn; actual FunctionDef is looked up at call time
            // so siblings can be bound then (enabling mutual recursion).
            let path: SmallVec<[StringId; 4]> = segments.into();
            Ok(Value::ModuleFn { path })
        }
        // Check for user module constant
        else if let Some(const_id) = self.env.get_user_module_const(segments)
        {
            Ok(self
                .arena
                .get(const_id)
                .cloned()
                .unwrap_or_else(|| invariant!("ValueId in arena")))
        } else {
            // Path starts with a module but doesn't resolve
            // Typechecker validates module paths
            typechecked!("module path", "valid member")
        }
    }

    /// Resolve a path as a type variant (for user-defined types).
    ///
    /// This handles paths like `Status.Pending` for types registered at
    /// runtime via `type` declarations.
    fn type_variant_path(
        &mut self,
        segments: &[StringId],
        _span: Span,
    ) -> Result<Value> {
        match segments {
            [ty_name, var_name] => {
                // Typechecker validates type names
                let type_id = self
                    .registry
                    .lookup(*ty_name)
                    .unwrap_or_else(|| typechecked!("type path", "known type"));

                // Typechecker validates variant names
                let v = self
                    .registry
                    .lookup_variant(type_id, *var_name)
                    .unwrap_or_else(|| {
                        typechecked!("type path", "known variant")
                    });

                let idx = v.idx;
                let ty_expr = self.build_variant_type_expr(type_id, idx, &[]);
                Ok(Value::Tagged(ty_expr, idx, smallvec::SmallVec::new()))
            }
            // Typechecker validates path structure
            _ => typechecked!("type path", "two segments"),
        }
    }
}
