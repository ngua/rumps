//! Module function and constant evaluation.
//!
//! Handles `Expr::Path` nodes that refer to module-qualified functions
//! (e.g., `Array.length`, `String.split`) or constants (e.g., `Math.pi`).
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
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Evaluate a namespace path to a module function.
    ///
    /// Handles paths of any length:
    /// - `Array.length` → `Value::ModuleFn { path: ["Array", "length"] }`
    /// - `Math.Trig.sin` → `Value::ModuleFn { path: ["Math", "Trig", "sin"] }`
    ///
    /// If the path doesn't resolve to a module function, falls back to
    /// treating it as a type variant path (for user-defined types registered
    /// at runtime).
    pub(super) fn path(
        &mut self,
        segments: &[String],
        span: Span,
    ) -> Result<Value> {
        // Need at least two segments: module + function (or type + variant)
        segments.split_first().map_or_else(
            || Err(Error::runtime(span, "empty path")),
            |(first, _)| {
                // Check if the first segment is a module
                if self.env.has_module(first) {
                    self.module_path(segments, span)
                } else {
                    // Fall back to type + variant interpretation
                    self.type_variant_path(segments, span)
                }
            },
        )
    }

    /// Resolve a path as a module function or constant.
    ///
    /// The path must have at least two segments. The last segment is the
    /// function/constant name; all preceding segments form the module path.
    /// Checks both builtin and user-defined modules.
    fn module_path(
        &mut self,
        segments: &[String],
        span: Span,
    ) -> Result<Value> {
        let path_strs: SmallVec<[&str; 4]> =
            segments.iter().map(String::as_str).collect();

        // Check for builtin module function first
        if self.env.module_fn_exists(&path_strs) {
            let path: SmallVec<[StringId; 4]> =
                segments.iter().map(|s| self.arena.intern(s)).collect();
            Ok(Value::ModuleFn { path })
        }
        // Check for builtin module constant
        else if let Some(const_id) = self.env.get_module_const(&path_strs) {
            self.env.consts.get(const_id).cloned().ok_or_else(|| {
                Error::runtime(span, "internal: missing constant")
            })
        }
        // Check for user module function
        else if let Some(fn_id) = self.env.get_user_module_fn(&path_strs) {
            // Get the closure value and clone it
            self.arena.get(fn_id).cloned().ok_or_else(|| {
                Error::runtime(span, "internal: missing user module function")
            })
        }
        // Check for user module constant
        else if let Some(const_id) =
            self.env.get_user_module_const(&path_strs)
        {
            self.arena.get(const_id).cloned().ok_or_else(|| {
                Error::runtime(span, "internal: missing user module constant")
            })
        } else {
            // Path starts with a module but doesn't resolve
            let path_str = segments.join(".");
            Err(Error::runtime(
                span,
                format!("unknown module member `{path_str}`"),
            ))
        }
    }

    /// Resolve a path as a type variant (for user-defined types).
    ///
    /// This handles paths like `Status.Pending` for types registered at
    /// runtime via `TYPE` declarations.
    fn type_variant_path(
        &mut self,
        segments: &[String],
        span: Span,
    ) -> Result<Value> {
        match segments {
            [ty_name, var_name] => {
                let ty_id = self.arena.intern(ty_name);
                let var_id = self.arena.intern(var_name);

                let type_id = self.registry.lookup(ty_id).ok_or_else(|| {
                    Error::runtime(
                        span,
                        format!("unknown type or module `{ty_name}`"),
                    )
                })?;

                let v = self
                    .registry
                    .lookup_variant(type_id, var_id)
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!(
                                "type `{ty_name}` has no variant `{var_name}`"
                            ),
                        )
                    })?;

                let idx = v.idx;
                let ty_expr = self.build_variant_type_expr(type_id, idx, &[]);
                Ok(Value::Tagged(ty_expr, idx, smallvec::SmallVec::new()))
            }
            _ => {
                let path_str = segments.join(".");
                Err(Error::runtime(
                    span,
                    format!(
                        "invalid path `{path_str}`; \
                         expected module function or type variant"
                    ),
                ))
            }
        }
    }
}
