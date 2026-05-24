//! Binary and unary operator implementations.

use async_recursion::async_recursion;
use ordered_float::OrderedFloat;

use super::class::ClassCtx;
use super::Interpreter;
use crate::ast::{BinOp, ExprId, UnOp};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::typecheck::Ty;
use crate::value::{Payload, ValueMeta};
use crate::{ClassId, Error, Result, Span};

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
            // Numeric class methods with Int fast-path
            BinOp::Add => match (left, right) {
                (Payload::Int(a), Payload::Int(b)) => {
                    Ok(Payload::Int(a.wrapping_add(*b)))
                }
                _ => {
                    let id = self.arena.intern("add");
                    self.dispatch_binary(
                        ClassId::NUMERIC,
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
                        ClassId::NUMERIC,
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
                        ClassId::NUMERIC,
                        id,
                        left,
                        right,
                        span,
                    )
                }
            },
            BinOp::Div => self.binop_div(left, right, span),
            BinOp::FloorDiv => {
                let id = self.arena.intern("floor-div");
                self.dispatch_binary(ClassId::NUMERIC, id, left, right, span)
            }
            BinOp::Mod => {
                let id = self.arena.intern("mod");
                self.dispatch_binary(ClassId::NUMERIC, id, left, right, span)
            }
            BinOp::Pow => {
                let id = self.arena.intern("pow");
                self.dispatch_binary(ClassId::NUMERIC, id, left, right, span)
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

            // Monoid class method
            BinOp::Concat => {
                let id = self.arena.intern("concat");
                self.dispatch_binary(ClassId::MONOID, id, left, right, span)
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
            ty_arena: &self.ty_arena,
            runtime_types: &self.runtime_types,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_binary(kind, mid, &mut ctx, left, right)
    }

    /// Dispatch a binary operator through user-defined class instance.
    ///
    /// Called when the typechecker recorded a user instance for this binary
    /// expression in `instance_calls`. Maps the operator to its class and
    /// method, then dispatches through `dispatch_class_method` which handles
    /// user instance lookup and invocation.
    #[async_recursion]
    pub(super) async fn dispatch_binop_user(
        &mut self,
        id: ExprId,
        left: &Payload,
        op: BinOp,
        right: &Payload,
        span: Span,
    ) -> Result<Payload> {
        let (class, method_str) = op.class_dispatch().unwrap_or_else(|| {
            typechecked!("binop user dispatch", "class-dispatched op")
        });
        let method = self.arena.intern(method_str);

        let l = self
            .arena
            .add_typed(left.clone(), ValueMeta::untyped(), span);
        let r = self
            .arena
            .add_typed(right.clone(), ValueMeta::untyped(), span);
        let result = self
            .dispatch_class_method(Some(id), class, method, &[l, r], span)
            .await?;

        // Post-process for operators that transform the class method result.
        // User `compare` returns `Ordering` (Tagged discriminant: `0`=Lt, `1`=Eq, `2`=Gt).
        match op {
            BinOp::Ne => match result {
                Payload::Bool(b) => Ok(Payload::Bool(!b)),
                _ => typechecked!("!=", "Bool"),
            },
            BinOp::Lt => match result {
                Payload::Tagged(_, d, _) => Ok(Payload::Bool(d == 0)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Gt => match result {
                Payload::Tagged(_, d, _) => Ok(Payload::Bool(d == 2)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Le => match result {
                Payload::Tagged(_, d, _) => Ok(Payload::Bool(d <= 1)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Ge => match result {
                Payload::Tagged(_, d, _) => Ok(Payload::Bool(d >= 1)),
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
                let ty_id = self
                    .checked_exprs
                    .get(&id)
                    .map(|info| info.ty.raw())
                    .unwrap_or_else(|| typechecked!("?", "resolved wrap type"));
                let ty = self.ty_arena.get(ty_id).clone();
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
            ty_arena: &self.ty_arena,
            runtime_types: &self.runtime_types,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
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
            ty_arena: &self.ty_arena,
            runtime_types: &self.runtime_types,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_convert(kind, mid, &mut ctx, v, target)
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

    /// Division (Float operands only).
    ///
    /// Type checker guarantees both operands are `Float`.
    /// Division by zero remains a runtime error (not type-level).
    fn binop_div(
        &self,
        left: &Payload,
        right: &Payload,
        span: Span,
    ) -> Result<Payload> {
        match (left, right) {
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat(a.0 / b.0)))
                }
            }
            _ => typechecked!("/", "Float"),
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
        let lhs_val = self.eval(lhs).await?;
        let rhs_val = self.eval(rhs).await?;

        // Coerce LHS to raw string (Stringable constraint verified by typechecker)
        let text = self.coerce_to_str(&lhs_val);

        // Get the regex cache index from RHS (typechecker guarantees Regex)
        let idx = match rhs_val {
            Payload::Regex(idx) => idx,
            _ => typechecked!("MATCHES", "Regex"),
        };
        let re = self.regex_cache.get(idx as usize).unwrap_or_else(|| {
            typechecked!("MATCHES regex", "valid cache index")
        });
        Ok(Payload::Bool(re.is_match(&text)))
    }
}
