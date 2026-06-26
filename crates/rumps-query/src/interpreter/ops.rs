//! Binary and unary operator implementations.

use std::cmp::Ordering;

use smallvec::SmallVec;

use super::{class, Interpreter};
use crate::ast::{BinOp, ExprId, UnOp};
use crate::intern::StringId;
use crate::typecheck::{RuntimeTyId, Ty, TyArena};
use crate::value::{Payload, TypeId, Value, ValueId};
use crate::{ClassId, Result, Span};

impl Interpreter<'_, '_> {
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
    pub(super) async fn apply_binop(
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
                    self.class_payload2(
                        ClassId::ADDITIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                    .await
                }
            },
            BinOp::Sub => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_sub(*b)))
                }
                _ => {
                    let id = self.arena.intern("sub");
                    self.class_payload2(
                        ClassId::SUBTRACTIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                    .await
                }
            },
            BinOp::Mul => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_mul(*b)))
                }
                _ => {
                    let id = self.arena.intern("mul");
                    self.class_payload2(
                        ClassId::MULTIPLICATIVE,
                        id,
                        left,
                        right,
                        span,
                    )
                    .await
                }
            },
            BinOp::Div => {
                let id = self.arena.intern("div");
                self.class_payload2(ClassId::DIVISIBLE, id, left, right, span)
                    .await
            }
            BinOp::FloorDiv => {
                let id = self.arena.intern("floor-div");
                self.class_payload2(
                    ClassId::FLOOR_DIVISIBLE,
                    id,
                    left,
                    right,
                    span,
                )
                .await
            }
            BinOp::Mod => {
                let id = self.arena.intern("mod");
                self.class_payload2(
                    ClassId::FLOOR_DIVISIBLE,
                    id,
                    left,
                    right,
                    span,
                )
                .await
            }
            BinOp::Pow => {
                let id = self.arena.intern("pow");
                self.class_payload2(ClassId::POWERABLE, id, left, right, span)
                    .await
            }

            // Equality via Eq class
            BinOp::Eq => {
                let id = self.arena.intern("eq");
                self.class_payload2(ClassId::EQ, id, left, right, span)
                    .await
            }
            BinOp::Ne => {
                let id = self.arena.intern("eq");
                let eq = self
                    .class_payload2(ClassId::EQ, id, left, right, span)
                    .await?;
                match eq {
                    Payload::Bool(b) => Ok(Payload::Bool(!b)),
                    _ => typechecked!("==", "Bool"),
                }
            }

            // Ord class methods with Int fast-path
            BinOp::Lt => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a < b)),
                _ => {
                    self.dispatch_compare(left, right, span, |ord| ord < 0)
                        .await
                }
            },
            BinOp::Gt => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a > b)),
                _ => {
                    self.dispatch_compare(left, right, span, |ord| ord > 0)
                        .await
                }
            },
            BinOp::Le => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a <= b)),
                _ => {
                    self.dispatch_compare(left, right, span, |ord| ord <= 0)
                        .await
                }
            },
            BinOp::Ge => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => Ok(Payload::Bool(a >= b)),
                _ => {
                    self.dispatch_compare(left, right, span, |ord| ord >= 0)
                        .await
                }
            },

            // Short-circuit ops handled elsewhere
            BinOp::And | BinOp::Or | BinOp::Coalesce | BinOp::Pipe => {
                unreachable!("handled in binary")
            }

            // Concatable class method
            BinOp::Concat => {
                let id = self.arena.intern("concat");
                self.class_payload2(ClassId::CONCATABLE, id, left, right, span)
                    .await
            }

            // BitLike class methods
            BinOp::BitAnd => {
                let id = self.arena.intern("bit-and");
                self.class_payload2(ClassId::BIT_LIKE, id, left, right, span)
                    .await
            }
            BinOp::BitOr => {
                let id = self.arena.intern("bit-or");
                self.class_payload2(ClassId::BIT_LIKE, id, left, right, span)
                    .await
            }
            BinOp::Shl => {
                let id = self.arena.intern("shl");
                self.class_payload2(ClassId::BIT_LIKE, id, left, right, span)
                    .await
            }
            BinOp::Shr => {
                let id = self.arena.intern("shr");
                self.class_payload2(ClassId::BIT_LIKE, id, left, right, span)
                    .await
            }
        }
    }

    pub(super) async fn apply_value_binop_async(
        &mut self,
        left: Value,
        op: BinOp,
        right: Value,
        span: Span,
    ) -> Result<Payload> {
        match op {
            BinOp::Eq | BinOp::Ne => {
                match self.fast_eq_payload(&left.payload, &right.payload) {
                    Some(b) => Ok(Payload::Bool(
                        if matches!(op, BinOp::Ne) { !b } else { b },
                    )),
                    None => {
                        let result = self
                            .dispatch_value_binop_method(left, op, right, span)
                            .await?;
                        self.binop_result_payload(op, result, span)
                    }
                }
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                match self.fast_ord_payload(&left.payload, &right.payload) {
                    Some(ord) => Ok(Payload::Bool(Self::ord_matches(op, ord))),
                    None => {
                        let result = self
                            .dispatch_value_binop_method(left, op, right, span)
                            .await?;
                        self.binop_result_payload(op, result, span)
                    }
                }
            }
            BinOp::Concat => {
                let result = self
                    .dispatch_value_binop_method(left, op, right, span)
                    .await?;
                self.binop_result_payload(op, result, span)
            }
            _ => typechecked!("value binop", "Eq, Ord, or Concatable"),
        }
    }

    fn fast_eq_payload(&self, left: &Payload, right: &Payload) -> Option<bool> {
        match (left, right) {
            (Payload::Unit, Payload::Unit) => Some(true),
            (Payload::Bool(a), Payload::Bool(b)) => Some(a == b),
            (Payload::Int(a), Payload::Int(b)) => Some(a == b),
            (Payload::Word(a), Payload::Word(b)) => Some(a == b),
            (Payload::Float(a), Payload::Float(b)) => Some(a == b),
            (Payload::Char(a), Payload::Char(b)) => Some(a == b),
            (Payload::String(a), Payload::String(b)) => Some(a == b),
            (Payload::Time(a), Payload::Time(b)) => Some(a == b),
            (Payload::FilePath(a), Payload::FilePath(b)) => Some(a == b),
            (Payload::Json(a), Payload::Json(b)) => Some(a == b),
            _ => None,
        }
    }

    fn fast_ord_payload(
        &self,
        left: &Payload,
        right: &Payload,
    ) -> Option<Ordering> {
        match (left, right) {
            (Payload::Bool(a), Payload::Bool(b)) => Some(a.cmp(b)),
            (Payload::Int(a), Payload::Int(b)) => Some(a.cmp(b)),
            (Payload::Word(a), Payload::Word(b)) => Some(a.cmp(b)),
            (Payload::Float(a), Payload::Float(b)) => Some(a.cmp(b)),
            (Payload::Char(a), Payload::Char(b)) => Some(a.cmp(b)),
            (Payload::String(a), Payload::String(b)) => {
                let a = self.arena.get_str(*a).unwrap_or("");
                let b = self.arena.get_str(*b).unwrap_or("");
                Some(a.cmp(b))
            }
            (Payload::Time(a), Payload::Time(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }

    async fn dispatch_value_binop_method(
        &mut self,
        left: Value,
        op: BinOp,
        right: Value,
        span: Span,
    ) -> Result<Value> {
        let output_ty = Self::binop_method_output_ty(&left, op);
        let (class, method_str) = op.class_dispatch().unwrap_or_else(|| {
            typechecked!("value binop dispatch", "class-dispatched op")
        });
        let method = self.arena.intern(method_str);
        let l = self.add_value(left, span);
        let r = self.add_value(right, span);
        self.dispatch_class_method_value(class::Dispatch {
            dispatch_expr_id: None,
            output_expr_id: None,
            output_ty: Some(output_ty),
            class,
            method,
            args: SmallVec::from_slice(&[l, r]),
            span,
        })
        .await
    }

    fn binop_method_output_ty(left: &Value, op: BinOp) -> RuntimeTyId {
        match op {
            BinOp::Eq | BinOp::Ne => RuntimeTyId::from(TyArena::BOOL),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                RuntimeTyId::from(TyArena::ORDERING)
            }
            BinOp::Concat => left.ty,
            _ => typechecked!("value binop dispatch", "class-dispatched op"),
        }
    }

    fn binop_result_payload(
        &mut self,
        op: BinOp,
        result: Value,
        span: Span,
    ) -> Result<Payload> {
        match op {
            BinOp::Ne => match result.payload {
                Payload::Bool(b) => Ok(Payload::Bool(!b)),
                _ => typechecked!("!=", "Bool"),
            },
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => self
                .result_ordering(result, span)
                .map(|ord| Payload::Bool(Self::ord_matches(op, ord))),
            _ => Ok(result.payload),
        }
    }

    fn result_ordering(
        &mut self,
        result: Value,
        _span: Span,
    ) -> Result<Ordering> {
        let ty = self
            .checked
            .types
            .to_type_id(result.repr)
            .or_else(|| self.checked.types.to_type_id(result.ty));
        match result.payload {
            Payload::Variant { tag: 0, .. }
                if ty.is_none_or(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Less)
            }
            Payload::Variant { tag: 1, .. }
                if ty.is_none_or(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Equal)
            }
            Payload::Variant { tag: 2, .. }
                if ty.is_none_or(|ty| ty == TypeId::ORDERING) =>
            {
                Ok(Ordering::Greater)
            }
            Payload::Int(n) if n < 0 => Ok(Ordering::Less),
            Payload::Int(0) => Ok(Ordering::Equal),
            Payload::Int(_) => Ok(Ordering::Greater),
            _ => typechecked!("compare result", "Ordering"),
        }
    }

    fn ord_matches(op: BinOp, ord: Ordering) -> bool {
        match op {
            BinOp::Lt => ord == Ordering::Less,
            BinOp::Gt => ord == Ordering::Greater,
            BinOp::Le => ord != Ordering::Greater,
            BinOp::Ge => ord != Ordering::Less,
            _ => typechecked!("comparison", "Ord operator"),
        }
    }

    async fn class_payload2(
        &mut self,
        kind: ClassId,
        mid: StringId,
        left: &Payload,
        right: &Payload,
        span: Span,
    ) -> Result<Payload> {
        let l = self.value_from_payload(left.clone());
        let r = self.value_from_payload(right.clone());
        let l = self.add_value(l, span);
        let r = self.add_value(r, span);
        self.class_payload(kind, mid, SmallVec::from_slice(&[l, r]), span, None)
            .await
    }

    /// Dispatch a binary operator through user-defined class instance.
    ///
    /// Maps the operator to its class and method, then dispatches through
    /// class method dispatch.
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
            .dispatch_class_method_value(class::Dispatch {
                dispatch_expr_id: Some(id),
                output_expr_id: Some(id),
                output_ty: None,
                class,
                method,
                args: SmallVec::from_slice(&[l, r]),
                span,
            })
            .await?;
        self.binop_result_payload(op, result, span)
            .map(|payload| self.value_for_expr(id, payload))
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
    pub(super) async fn apply_unop(
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
                    self.class_payload1(ClassId::NEGATABLE, id, &v, span).await
                }
            },
            UnOp::Not => Ok(match &v {
                Payload::Bool(b) => Payload::Bool(!b),
                _ => typechecked!("NOT", "Bool"),
            }),
            UnOp::Wrap => {
                // Look up target type (defaulted to `Option[T]` during constraint solving)
                let ty_id = self.checked.expr(id).ty;
                let mid = self.arena.intern("wrap");
                self.class_convert_payload(
                    ClassId::WRAPPABLE,
                    mid,
                    &v,
                    ty_id,
                    span,
                )
                .await
            }
        }
    }

    async fn class_payload1(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Payload,
        span: Span,
    ) -> Result<Payload> {
        let v = self.value_from_payload(v.clone());
        let id = self.add_value(v, span);
        self.class_payload(kind, mid, SmallVec::from_slice(&[id]), span, None)
            .await
    }

    async fn class_convert_payload(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Payload,
        target: RuntimeTyId,
        span: Span,
    ) -> Result<Payload> {
        let v = match (kind, v, self.checked.types.get(target)) {
            (
                ClassId::WRAPPABLE,
                Payload::Variant { .. },
                Ty::Option(inner) | Ty::Result(inner, _),
            ) => {
                let meta = self.checked.types.meta(RuntimeTyId::from(*inner));
                self.value_from_meta(v.clone(), meta)
            }
            _ => self.value_from_payload(v.clone()),
        };
        let id = self.add_value(v, span);
        self.class_payload(
            kind,
            mid,
            SmallVec::from_slice(&[id]),
            span,
            Some(target),
        )
        .await
    }

    pub(super) async fn class_convert_value(
        &mut self,
        kind: ClassId,
        mid: StringId,
        v: &Value,
        target: &Ty,
        span: Span,
    ) -> Result<Payload> {
        let target = self.checked.types.intern(target.clone());
        let id = self.add_value(v.clone(), span);
        self.class_payload(
            kind,
            mid,
            SmallVec::from_slice(&[id]),
            span,
            Some(target),
        )
        .await
    }

    /// Dispatch `Ord:compare` and apply a predicate to the result.
    ///
    /// The predicate receives the ordering as `Int` (-1, 0, 1) and returns
    /// whether the comparison is satisfied.
    async fn dispatch_compare<F>(
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
        let ord = self
            .class_payload2(ClassId::ORD, cmp, left, right, span)
            .await?;
        match ord {
            Payload::Int(n) => Ok(Payload::Bool(pred(n))),
            _ => typechecked!("compare result", "Int"),
        }
    }

    async fn class_payload(
        &mut self,
        kind: ClassId,
        mid: StringId,
        args: SmallVec<[ValueId; 4]>,
        span: Span,
        output_ty: Option<RuntimeTyId>,
    ) -> Result<Payload> {
        self.dispatch_class_method_value(class::Dispatch::internal(
            kind, mid, args, output_ty, span,
        ))
        .await
        .map(|v| v.payload)
    }

    /// Evaluate a `MATCHES` expression.
    ///
    /// Tests a string LHS against the RHS regex pattern.
    /// Type checker guarantees RHS is a `Regex` value.
    pub(super) async fn matches(
        &mut self,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<Payload> {
        let lhs_val = self.eval_payload(lhs).await?;
        let rhs_val = self.eval_payload(rhs).await?;

        let text = match lhs_val {
            Payload::String(id) => {
                self.arena.get_str(id).unwrap_or_default().to_owned()
            }
            _ => typechecked!("MATCHES", "Into[String] returned String"),
        };

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
