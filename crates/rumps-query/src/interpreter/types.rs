//! Type checking, matching, and validation.

use super::Interpreter;
use crate::io::IoContext;
use crate::value::{Payload, TypeDef, TypeId, Value};
use crate::{ClassId, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Perform type coercion for `AS` casts with full value metadata.
    pub(super) fn coerce_value(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Payload> {
        let is_wrap = self.registry.get_def(target).is_some_and(|def| {
            matches!(def, TypeDef::Union { .. } | TypeDef::Alias { .. })
        });
        if is_wrap {
            Ok(val.payload.clone())
        } else {
            let ty_id = self.checked.types.type_id(target);
            let ty = self.checked.types.get(ty_id).clone();
            let mid = self.arena.intern("into");
            self.dispatch_convert_value(ClassId::INTO, mid, val, &ty, span)
        }
    }

    /// Extract the `Ok` value from a `Result`.
    ///
    /// Callers must ensure this is only invoked on `Result.Ok` values.
    pub(super) fn unwrap_result_ok(&self, result: &Payload) -> Result<Payload> {
        match result {
            Payload::Variant { tag: 0, vals } => Ok(vals
                .first()
                .and_then(|id| self.arena.payload(*id).cloned())
                .unwrap_or_else(|| typechecked!("Result.Ok", "payload"))),
            _ => typechecked!("unwrap_result_ok", "Result.Ok"),
        }
    }

    /// Check whether a generated `read` result payload is `Result.Ok`.
    pub(super) fn result_payload_is_ok(&self, result: &Payload) -> bool {
        matches!(result, Payload::Variant { tag: 0, .. })
    }

    /// Extract the error message from a `Result.Err`.
    pub(super) fn extract_result_err_msg(&self, result: &Payload) -> String {
        match result {
            Payload::Variant { tag: 1, vals } => vals
                .first()
                .and_then(|id| self.arena.payload(*id))
                .and_then(|v| match v {
                    Payload::String(sid) => {
                        self.arena.get_str(*sid).map(str::to_owned)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| "unknown error".to_owned()),
            _ => "unknown error".to_owned(),
        }
    }
}
