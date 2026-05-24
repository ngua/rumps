//! Type checking, matching, and validation.

use super::Interpreter;
use crate::io::IoContext;
use crate::typecheck::{RuntimeTyId, Ty, TyArena, TyId};
use crate::value::{Payload, TypeDef, TypeId};
use crate::{ClassId, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Determine the `RuntimeTyId` for a `Payload` from its variant.
    ///
    /// For primitives, returns the pre-interned constant. For parameterized
    /// and compound types (arrays, objects, tuples, etc.), returns `UNKNOWN`
    /// as their `RuntimeTyId` requires type args not stored in the `Payload`.
    /// For `Tagged` variants with known non-parameterized sum types, returns
    /// the corresponding constant; otherwise `UNKNOWN`.
    pub(super) fn payload_runtime_ty(&self, v: &Payload) -> RuntimeTyId {
        match v {
            Payload::Unit => RuntimeTyId::from(TyArena::UNIT),
            Payload::Bool(_) => RuntimeTyId::from(TyArena::BOOL),
            Payload::Int(_) => RuntimeTyId::from(TyArena::INT),
            Payload::Word(_) => RuntimeTyId::from(TyArena::WORD),
            Payload::Float(_) => RuntimeTyId::from(TyArena::FLOAT),
            Payload::Char(_) => RuntimeTyId::from(TyArena::CHAR),
            Payload::String(_) => RuntimeTyId::from(TyArena::STRING),
            Payload::FilePath(_) => RuntimeTyId::from(TyArena::FILEPATH),
            Payload::Regex(_) => RuntimeTyId::from(TyArena::REGEX),
            Payload::Time(_) => RuntimeTyId::from(TyArena::TIME),
            Payload::Json(_) => RuntimeTyId::from(TyArena::JSON),
            Payload::Range { .. } => RuntimeTyId::from(TyArena::RANGE),
            Payload::ForeverContinuation | Payload::LoopContinue(_) => {
                RuntimeTyId::from(TyArena::UNIT)
            }
            Payload::Ref(is_global, ..) => {
                if *is_global {
                    RuntimeTyId::from(TyArena::GLOBAL)
                } else {
                    RuntimeTyId::from(TyArena::LOCAL)
                }
            }
            Payload::Tagged(ty, ..) => match *ty {
                TypeId::ORDERING => RuntimeTyId::from(TyArena::ORDERING),
                TypeId::DATA_STATUS => RuntimeTyId::from(TyArena::DATA_STATUS),
                TypeId::PATH => RuntimeTyId::from(TyArena::PATH),
                TypeId::ERROR => RuntimeTyId::from(TyArena::ERROR),
                _ => RuntimeTyId::UNKNOWN,
            },
            _ => RuntimeTyId::UNKNOWN,
        }
    }

    /// Perform type coercion for `AS` casts via `Into[T]` dispatch.
    ///
    /// When the target is a union or alias, returns the value as-is (the
    /// `ValueMeta` tracks the type). For other types, dispatches through
    /// `Into[T]`.
    ///
    /// EXCEPTION: `Storable AS T` must remain a runtime error (handled by
    /// `TryInto`).
    pub(super) fn coerce(
        &mut self,
        val: &Payload,
        target: TypeId,
        span: Span,
    ) -> Result<Payload> {
        let is_wrap = self.registry.get_def(target).is_some_and(|def| {
            matches!(def, TypeDef::Union { .. } | TypeDef::Alias { .. })
        });
        if is_wrap {
            Ok(val.clone())
        } else {
            let ty_id = Self::type_id_to_ty_id(target, &mut self.ty_arena);
            let ty = self.ty_arena.get(ty_id).clone();
            let mid = self.arena.intern("into");
            self.dispatch_convert(ClassId::INTO, mid, val, &ty, span)
        }
    }

    /// Convert a runtime `TypeId` to the corresponding `TyId` in the type arena.
    pub(super) fn type_id_to_ty_id(id: TypeId, ta: &mut TyArena) -> TyId {
        match id {
            TypeId::UNIT => TyArena::UNIT,
            TypeId::BOOL => TyArena::BOOL,
            TypeId::INT => TyArena::INT,
            TypeId::WORD => TyArena::WORD,
            TypeId::FLOAT => TyArena::FLOAT,
            TypeId::CHAR => TyArena::CHAR,
            TypeId::STRING => TyArena::STRING,
            TypeId::FILEPATH => TyArena::FILEPATH,
            TypeId::JSON => TyArena::JSON,
            TypeId::TIME => TyArena::TIME,
            TypeId::RANGE => TyArena::RANGE,
            TypeId::ORDERING => TyArena::ORDERING,
            TypeId::DATA_STATUS => TyArena::DATA_STATUS,
            TypeId::PATH => TyArena::PATH,
            TypeId::REGEX => TyArena::REGEX,
            TypeId::LOCAL => TyArena::LOCAL,
            TypeId::GLOBAL => TyArena::GLOBAL,
            other => ta.alloc(Ty::Named(other, smallvec::smallvec![])),
        }
    }

    /// Perform fallible type conversion for `read` via `TryInto[T]` dispatch.
    ///
    /// This is the runtime helper for `expr READ Type` syntax.
    /// Returns a RUMPS `Result[T, String]` value (not `crate::Result`).
    pub(super) fn read_value(
        &mut self,
        val: &Payload,
        target: TypeId,
        span: Span,
    ) -> Result<Payload> {
        let ty_id = Self::type_id_to_ty_id(target, &mut self.ty_arena);
        let ty = self.ty_arena.get(ty_id).clone();
        let mid = self.arena.intern("try-into");
        self.dispatch_convert(ClassId::TRY_INTO, mid, val, &ty, span)
    }

    /// Extract the `Ok` value from a `Result`.
    ///
    /// Callers must ensure this is only invoked on `Result.Ok` values.
    pub(super) fn unwrap_result_ok(
        &self,
        result: &Payload,
        _span: Span,
    ) -> Result<Payload> {
        match result {
            Payload::Tagged(ty, 0, payloads) if *ty == TypeId::RESULT => {
                Ok(payloads
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| typechecked!("Result.Ok", "payload")))
            }
            _ => typechecked!("unwrap_result_ok", "Result.Ok"),
        }
    }

    /// Extract the error message from a `Result.Err`.
    pub(super) fn extract_result_err_msg(&self, result: &Payload) -> String {
        match result {
            Payload::Tagged(ty, 1, payloads) if *ty == TypeId::RESULT => {
                payloads
                    .first()
                    .and_then(|id| self.arena.get(*id))
                    .and_then(|v| match v {
                        Payload::String(sid) => {
                            self.arena.get_str(*sid).map(str::to_owned)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "unknown error".to_owned())
            }
            _ => "unknown error".to_owned(),
        }
    }
}
