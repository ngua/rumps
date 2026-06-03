//! Binary and unary operator implementations.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::call::ClassDispatch;
use super::class::{self, ClassCtx};
use super::Interpreter;
use crate::ast::{BinOp, ExprId, UnOp};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::typecheck::Ty;
use crate::value::{Payload, TypeId, Value};
use crate::{ClassId, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Apply a binary operation to two values.
    ///
    /// Type checker guarantees operand types match the operator requirements.
    /// Division/modulo by zero remain runtime errors (not type-level).
    ///
    /// # Fast-paths
    ///
    /// Common operations on `Int` are inlined to avoid class dispatch overhead.
    /// This covers ~90% of arithmetic in typical scripts. All other cases fall
    /// through to class method dispatch.
    pub(super) fn apply_binop(
        &mut self,
        left: &Payload,
        op: BinOp,
        right: &Payload,
        span: Span,
    ) -> Result<Payload> {
        match op {
            // Arithmetic class methods with `Int` fast-path
            BinOp::Add => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_add(*b)))
                }
                _ => {
                    let id = self.arena.intern("add");
                    self.dispatch_binary(
                        ClassId::ADDITIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                }
            },
            BinOp::Sub => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_sub(*b)))
                }
                _ => {
                    let id = self.arena.intern("sub");
                    self.dispatch_binary(
                        ClassId::SUBTRACTIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                }
            },
            BinOp::Mul => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_mul(*b)))
                }
                _ => {
                    let id = self.arena.intern("mul");
                    self.dispatch_binary(
                        ClassId::MULTIPLICATIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                }
            },
            BinOp::Div => {
                let id = self.arena.intern("div");
                self.dispatch_binary(ClassId::DIVISIBLE, id, left, right, span)
            }
            BinOp::FloorDiv => {
                let id = self.arena.intern("floor-div");
                self.dispatch_binary(
                    ClassId::FLOOR_DIVISIBLE,
                    id,
                    left,
                    right,
                    span,
                )
            }
            BinOp::Mod => {
                let id = self.arena.intern("mod");
                self.dispatch_binary(
                    ClassId::FLOOR_DIVISIBLE,
                    id,
                    left,
                    right,
                    span,
                )
            }
            BinOp::Pow => {
                let id = self.arena.intern("pow");
                self.dispatch_binary(ClassId::POWERABLE, id, left, right, span)
            }

            // Equality via Eq class
            BinOp::Eq => {
                let id = self.arena.intern("eq");
                self.dispatch_binary(ClassId::EQ, id, left, right, span)
            }
            BinOp::Ne => {
                let id = self.arena.intern("eq");
                let eq =
                    self.dispatch_binary(ClassId::EQ, id, left, right, span)?;
                match eq {
                    Payload::Bool(b) => Ok(Payload::Bool(!b)),
                    _ => typechecked!("==", "Bool"),
                }
            }

            // Ord class methods with Int fast-path
            BinOp::Lt => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a < b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord < 0),
            },
            BinOp::Gt => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a > b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord > 0),
            },
            BinOp::Le => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a <= b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord <= 0),
            },
            BinOp::Ge => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a >= b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord >= 0),
            },

            // Short-circuit ops handled elsewhere
            BinOp::And | BinOp::Or | BinOp::Coalesce | BinOp::Pipe => {
                unreachable!("handled in binary")
            }

            // Concatable class method
            BinOp::Concat => {
                let id = self.arena.intern("concat");
                self.dispatch_binary(ClassId::CONCATABLE, id, left, right, span)
            }

            // BitLike class methods
            BinOp::BitAnd => {
                let id = self.arena.intern("bit-and");
                self.dispatch_binary(ClassId::BIT_LIKE, id, left, right, span)
            }
            BinOp::BitOr => {
                let id = self.arena.intern("bit-or");
                self.dispatch_binary(ClassId::BIT_LIKE, id, left, right, span)
            }
            BinOp::Shl => {
                let id = self.arena.intern("shl");
                self.dispatch_binary(ClassId::BIT_LIKE, id, left, right, span)
            }
            BinOp::Shr => {
                let id = self.arena.intern("shr");
                self.dispatch_binary(ClassId::BIT_LIKE, id, left, right, span)
            }
        }
    }

    pub(super) fn apply_ord_binop(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Payload> {
        match (&left.payload, &right.payload, op) {
            (Payload::Int(a), Payload::Int(b), BinOp::Lt) => {
                Ok(Payload::Bool(a < b))
            }
            (Payload::Int(a), Payload::Int(b), BinOp::Gt) => {
                Ok(Payload::Bool(a > b))
            }
            (Payload::Int(a), Payload::Int(b), BinOp::Le) => {
                Ok(Payload::Bool(a <= b))
            }
            (Payload::Int(a), Payload::Int(b), BinOp::Ge) => {
                Ok(Payload::Bool(a >= b))
            }
            (_, _, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge) => {
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                let ord = class::Ord::compare_values(&mut ctx, left, right);
                match ord {
                    Payload::Int(n) => Ok(Payload::Bool(match op {
                        BinOp::Lt => n < 0,
                        BinOp::Gt => n > 0,
                        BinOp::Le => n <= 0,
                        BinOp::Ge => n >= 0,
                        _ => typechecked!("comparison", "Ord operator"),
                    })),
                    _ => typechecked!("compare result", "Int"),
                }
            }
            _ => typechecked!("comparison", "Ord operator"),
        }
    }

    pub(super) fn apply_value_binop(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Payload> {
        match op {
            BinOp::Eq | BinOp::Ne => {
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                match class::Eq::eq_values(&mut ctx, left, right) {
                    Payload::Bool(b) if matches!(op, BinOp::Ne) => {
                        Ok(Payload::Bool(!b))
                    }
                    payload => Ok(payload),
                }
            }
            BinOp::Concat => {
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                class::Concatable::concat_values(&mut ctx, left, right)
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.apply_ord_binop(left, op, right, span)
            }
            _ => typechecked!("value binop", "Eq, Ord, or Concatable"),
        }
    }

    /// Dispatch a binary class method.
    fn dispatch_binary(
        &mut self,
        kind: ClassId,
        mid: StringId,
        left: &Payload,
        right: &Payload,
        span: Span,
    ) -> Result<Payload> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_binary(kind, mid, &mut ctx, left, right)
    }

    /// Dispatch a binary operator through user-defined class instance.
    ///
    /// Maps the operator to its class and method, then dispatches through
    /// class method dispatch.
    #[async_recursion]
    pub(super) async fn dispatch_binop_user(
        &mut self,
        id: ExprId,
        left: Value,
        op: BinOp,
        right: Value,
        span: Span,
    ) -> Result<Value> {
        let (class, method_str) = op.class_dispatch().unwrap_or_else(|| {
            typechecked!("binop user dispatch", "class-dispatched op")
        });
        let method = self.arena.intern(method_str);

        let l = self.add_value(left, span);
        let r = self.add_value(right, span);
        let result = self
            .dispatch_class_method_value(ClassDispatch {
                dispatch_expr_id: Some(id),
                output_expr_id: Some(id),
                class,
                method,
                args: SmallVec::from_slice(&[l, r]),
                span,
            })
            .await?;

        // Post-process for operators that transform the class method result.
        // User `compare` returns `Ordering`; tags are `0`=Lt, `1`=Eq, `2`=Gt.
        let result_ty = self
            .checked
            .types
            .to_type_id(result.repr)
            .or_else(|| self.checked.types.to_type_id(result.ty));
        match op {
            BinOp::Ne => match result.payload {
                Payload::Bool(b) => {
                    Ok(self.value_for_expr(id, Payload::Bool(!b)))
                }
                _ => typechecked!("!=", "Bool"),
            },
            BinOp::Lt => match (result_ty, result.payload) {
                (Some(TypeId::ORDERING), Payload::Variant { tag, .. }) => {
                    Ok(self.value_for_expr(id, Payload::Bool(tag == 0)))
                }
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Gt => match (result_ty, result.payload) {
                (Some(TypeId::ORDERING), Payload::Variant { tag, .. }) => {
                    Ok(self.value_for_expr(id, Payload::Bool(tag == 2)))
                }
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Le => match (result_ty, result.payload) {
                (Some(TypeId::ORDERING), Payload::Variant { tag, .. }) => {
                    Ok(self.value_for_expr(id, Payload::Bool(tag <= 1)))
                }
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Ge => match (result_ty, result.payload) {
                (Some(TypeId::ORDERING), Payload::Variant { tag, .. }) => {
                    Ok(self.value_for_expr(id, Payload::Bool(tag >= 1)))
                }
                _ => typechecked!("compare result", "Ordering"),
            },
            _ => Ok(result),
        }
    }

    /// Unary operation application.
    ///
    /// Type checker guarantees:
    /// - `-` is only applied to `Negatable` types (`Int` or `Float`)
    /// - `NOT` is only applied to `Bool`
    /// - `?` wraps in `Option.Some` or `Result.Ok` depending on context
    ///
    /// # Fast-paths
    ///
    /// Negation on `Int` is inlined to avoid class dispatch overhead.
    pub(super) fn apply_unop(
        &mut self,
        id: ExprId,
        op: UnOp,
        v: Payload,
        span: Span,
    ) -> Result<Payload> {
        match op {
            UnOp::Neg => match &v {
                // Fast-path: Int negation (most common)
                Payload::Int(n) => Ok(Payload::Int(-n)),
                _ => {
                    let id = self.arena.intern("neg");
                    self.dispatch_unary(ClassId::NEGATABLE, id, &v, span)
                }
            },
            UnOp::Not => Ok(match &v {
                Payload::Bool(b) => Payload::Bool(!b),
                _ => typechecked!("NOT", "Bool"),
            }),
            UnOp::Wrap => {
                // Look up target type (defaulted to `Option[T]` during constraint solving)
                let ty_id = self.checked.expr(id).ty;
                let ty = self.checked.types.get(ty_id).clone();
                let mid = self.arena.intern("wrap");
                self.dispatch_convert(ClassId::WRAPPABLE, mid, &v, &ty, span)
            }
        }
    }

    /// Dispatch a unary class method.
    pub(super) fn dispatch_unary(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Payload,
        span: Span,
    ) -> Result<Payload> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        self.class_methods.dispatch_unary(kind, mid, &mut ctx, v)
    }

    /// Dispatch a conversion class method (`Into:into`, `TryInto:try-into`).
    pub(super) fn dispatch_convert(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Payload,
        target: &Ty,
        span: Span,
    ) -> Result<Payload> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_convert(kind, mid, &mut ctx, v, target)
    }

    /// Dispatch a conversion class method with full value metadata.
    pub(super) fn dispatch_convert_value(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Value,
        target: &Ty,
        span: Span,
    ) -> Result<Payload> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        match kind {
            ClassId::INTO => class::Into::into_value(&mut ctx, v, target),
            ClassId::TRY_INTO => {
                class::TryInto::try_into_value(&mut ctx, v, target)
            }
            _ => self
                .class_methods
                .dispatch_convert(kind, mid, &mut ctx, &v.payload, target),
        }
    }

    /// Dispatch `Ord:compare` and apply a predicate to the result.
    ///
    /// The predicate receives the ordering as `Int` (-1, 0, 1) and returns
    /// whether the comparison is satisfied.
    fn dispatch_compare<F>(
        &mut self,
        left: &Payload,
        right: &Payload,
        span: Span,
        pred: F,
    ) -> Result<Payload>
    where
        F: FnOnce(i64) -> bool,
    {
        let cmp = self.arena.intern("compare");
        let ord = self.dispatch_binary(ClassId::ORD, cmp, left, right, span)?;
        match ord {
            Payload::Int(n) => Ok(Payload::Bool(pred(n))),
            _ => typechecked!("compare result", "Int"),
        }
    }

    /// Evaluate a `MATCHES` expression.
    ///
    /// Stringifies the LHS and tests it against the RHS regex pattern.
    /// Type checker guarantees RHS is a `Regex` value.
    pub(super) async fn matches(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Payload> {
        let lhs_val = self.eval_payload(lhs).await?;
        let rhs_val = self.eval_payload(rhs).await?;

        // Coerce LHS to raw string (Stringable constraint verified by typechecker)
        let text = self.coerce_to_str(&lhs_val);

        // Get the regex cache index from RHS (typechecker guarantees Regex)
        let idx = match rhs_val {
            Payload::Regex(idx) => idx,
            _ => typechecked!("MATCHES", "Regex"),
        };
        let re =
            self.checked
                .regex_cache
                .get(idx as usize)
                .unwrap_or_else(|| {
                    typechecked!("MATCHES regex", "valid cache index")
                });
        Ok(Payload::Bool(re.is_match(&text)))
    }
}
