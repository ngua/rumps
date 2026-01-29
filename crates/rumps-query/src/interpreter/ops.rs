//! Binary and unary operator implementations.

use ordered_float::OrderedFloat;

use super::class::ClassCtx;
use super::Interpreter;
use crate::ast::{BinOp, ExprId, UnOp};
use crate::io::IoContext;
use crate::typecheck::ClassKind;
use crate::value::Value;
use crate::{Error, Result, Span};

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
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            // Numeric class methods with Int fast-path
            BinOp::Add => match (left, right) {
                (Value::Int(a), Value::Int(b)) => {
                    Ok(Value::Int(a.wrapping_add(*b)))
                }
                _ => self.dispatch_binary(
                    ClassKind::Numeric,
                    "add",
                    left,
                    right,
                    span,
                ),
            },
            BinOp::Sub => match (left, right) {
                (Value::Int(a), Value::Int(b)) => {
                    Ok(Value::Int(a.wrapping_sub(*b)))
                }
                _ => self.dispatch_binary(
                    ClassKind::Numeric,
                    "sub",
                    left,
                    right,
                    span,
                ),
            },
            BinOp::Mul => match (left, right) {
                (Value::Int(a), Value::Int(b)) => {
                    Ok(Value::Int(a.wrapping_mul(*b)))
                }
                _ => self.dispatch_binary(
                    ClassKind::Numeric,
                    "mul",
                    left,
                    right,
                    span,
                ),
            },
            BinOp::Div => self.binop_div(left, right, span),
            BinOp::FloorDiv => self.dispatch_binary(
                ClassKind::Numeric,
                "floor-div",
                left,
                right,
                span,
            ),
            BinOp::Mod => self.dispatch_binary(
                ClassKind::Numeric,
                "mod",
                left,
                right,
                span,
            ),
            BinOp::Pow => self.dispatch_binary(
                ClassKind::Numeric,
                "pow",
                left,
                right,
                span,
            ),

            // Equality via Eq class
            BinOp::Eq => {
                self.dispatch_binary(ClassKind::Eq, "eq", left, right, span)
            }
            BinOp::Ne => {
                let eq = self.dispatch_binary(
                    ClassKind::Eq,
                    "eq",
                    left,
                    right,
                    span,
                )?;
                match eq {
                    Value::Bool(b) => Ok(Value::Bool(!b)),
                    _ => typechecked!("==", "Bool"),
                }
            }

            // Ord class methods with Int fast-path
            BinOp::Lt => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord < 0),
            },
            BinOp::Gt => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord > 0),
            },
            BinOp::Le => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord <= 0),
            },
            BinOp::Ge => match (left, right) {
                (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
                _ => self.dispatch_compare(left, right, span, |ord| ord >= 0),
            },

            // Short-circuit ops handled elsewhere
            BinOp::And | BinOp::Or | BinOp::Coalesce | BinOp::Pipe => {
                unreachable!("handled in binary")
            }

            // Monoid class method
            BinOp::Concat => self.dispatch_binary(
                ClassKind::Monoid,
                "concat",
                left,
                right,
                span,
            ),

            // BitLike class methods
            BinOp::BitAnd => self.dispatch_binary(
                ClassKind::BitLike,
                "bit-and",
                left,
                right,
                span,
            ),
            BinOp::BitOr => self.dispatch_binary(
                ClassKind::BitLike,
                "bit-or",
                left,
                right,
                span,
            ),
            BinOp::Shl => self.dispatch_binary(
                ClassKind::BitLike,
                "shl",
                left,
                right,
                span,
            ),
            BinOp::Shr => self.dispatch_binary(
                ClassKind::BitLike,
                "shr",
                left,
                right,
                span,
            ),
        }
    }

    /// Dispatch a binary class method.
    fn dispatch_binary(
        &mut self,
        kind: ClassKind,
        method: &str,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_binary(kind, method, &mut ctx, left, right)
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
        v: Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            UnOp::Neg => match &v {
                // Fast-path: Int negation (most common)
                Value::Int(n) => Ok(Value::Int(-n)),
                _ => self.dispatch_unary(ClassKind::Negatable, "neg", &v, span),
            },
            UnOp::Not => Ok(match &v {
                Value::Bool(b) => Value::Bool(!b),
                _ => typechecked!("NOT", "Bool"),
            }),
            UnOp::Wrap => {
                // Look up target type (defaulted to `Option[T]` during constraint solving)
                let ty =
                    self.wrap_types.get(&id).cloned().unwrap_or_else(|| {
                        typechecked!("?", "resolved wrap type")
                    });
                self.dispatch_convert(
                    ClassKind::Fallible,
                    "wrap",
                    &v,
                    &ty,
                    span,
                )
            }
        }
    }

    /// Dispatch a unary class method.
    pub(super) fn dispatch_unary(
        &mut self,
        kind: ClassKind,
        method: &str,
        v: &Value,
        span: Span,
    ) -> Result<Value> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods.dispatch_unary(kind, method, &mut ctx, v)
    }

    /// Dispatch a conversion class method (`Into:into`, `TryInto:try-into`).
    pub(super) fn dispatch_convert(
        &mut self,
        kind: ClassKind,
        method: &str,
        v: &Value,
        target: &crate::typecheck::Ty,
        span: Span,
    ) -> Result<Value> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_convert(kind, method, &mut ctx, v, target)
    }

    /// Dispatch `Ord:compare` and apply a predicate to the result.
    ///
    /// The predicate receives the ordering as `Int` (-1, 0, 1) and returns
    /// whether the comparison is satisfied.
    fn dispatch_compare<F>(
        &mut self,
        left: &Value,
        right: &Value,
        span: Span,
        pred: F,
    ) -> Result<Value>
    where
        F: FnOnce(i64) -> bool,
    {
        let ord =
            self.dispatch_binary(ClassKind::Ord, "compare", left, right, span)?;
        match ord {
            Value::Int(n) => Ok(Value::Bool(pred(n))),
            _ => typechecked!("compare result", "Int"),
        }
    }

    /// Division (Float operands only).
    ///
    /// Type checker guarantees both operands are `Float`.
    /// Division by zero remains a runtime error (not type-level).
    fn binop_div(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Float(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(a.0 / b.0)))
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
        lhs: crate::ast::ExprId,
        rhs: crate::ast::ExprId,
    ) -> Result<Value> {
        let lhs_val = self.eval(lhs).await?;
        let rhs_val = self.eval(rhs).await?;

        // Coerce LHS to raw string (Stringable constraint verified by typechecker)
        let text = self.coerce_to_str(&lhs_val);

        // Get the regex cache index from RHS (typechecker guarantees Regex)
        let idx = match rhs_val {
            Value::Regex(idx) => idx,
            _ => typechecked!("MATCHES", "Regex"),
        };
        let re = self.regex_cache.get(idx as usize).unwrap_or_else(|| {
            typechecked!("MATCHES regex", "valid cache index")
        });
        Ok(Value::Bool(re.is_match(&text)))
    }
}
