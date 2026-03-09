//! Binary and unary operator implementations.

use async_recursion::async_recursion;
use ordered_float::OrderedFloat;

use super::class::ClassCtx;
use super::Interpreter;
use crate::ast::{BinOp, ExprId, UnOp};
use crate::io::IoContext;
use crate::typecheck::BuiltinClassTag;
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
                    BuiltinClassTag::Numeric,
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
                    BuiltinClassTag::Numeric,
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
                    BuiltinClassTag::Numeric,
                    "mul",
                    left,
                    right,
                    span,
                ),
            },
            BinOp::Div => self.binop_div(left, right, span),
            BinOp::FloorDiv => self.dispatch_binary(
                BuiltinClassTag::Numeric,
                "floor-div",
                left,
                right,
                span,
            ),
            BinOp::Mod => self.dispatch_binary(
                BuiltinClassTag::Numeric,
                "mod",
                left,
                right,
                span,
            ),
            BinOp::Pow => self.dispatch_binary(
                BuiltinClassTag::Numeric,
                "pow",
                left,
                right,
                span,
            ),

            // Equality via Eq class
            BinOp::Eq => self.dispatch_binary(
                BuiltinClassTag::Eq,
                "eq",
                left,
                right,
                span,
            ),
            BinOp::Ne => {
                let eq = self.dispatch_binary(
                    BuiltinClassTag::Eq,
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
                BuiltinClassTag::Monoid,
                "concat",
                left,
                right,
                span,
            ),

            // BitLike class methods
            BinOp::BitAnd => self.dispatch_binary(
                BuiltinClassTag::BitLike,
                "bit-and",
                left,
                right,
                span,
            ),
            BinOp::BitOr => self.dispatch_binary(
                BuiltinClassTag::BitLike,
                "bit-or",
                left,
                right,
                span,
            ),
            BinOp::Shl => self.dispatch_binary(
                BuiltinClassTag::BitLike,
                "shl",
                left,
                right,
                span,
            ),
            BinOp::Shr => self.dispatch_binary(
                BuiltinClassTag::BitLike,
                "shr",
                left,
                right,
                span,
            ),
        }
    }

    /// Dispatch a binary class method.
    ///
    /// Unwraps Union/Newtype values before dispatching to allow auto-derivation
    /// of class methods for user-defined types.
    fn dispatch_binary(
        &mut self,
        kind: BuiltinClassTag,
        method: &str,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        // Unwrap Union/Newtype to auto-derive class methods
        let unwrapped_l = self.unwrap_value_recursive(left);
        let l = unwrapped_l.as_ref().unwrap_or(left);
        let unwrapped_r = self.unwrap_value_recursive(right);
        let r = unwrapped_r.as_ref().unwrap_or(right);

        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_binary(kind, method, &mut ctx, l, r)
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
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        let (class, method) = op.class_dispatch().unwrap_or_else(|| {
            typechecked!("binop user dispatch", "class-dispatched op")
        });

        let l = self.arena.add(left.clone(), span);
        let r = self.arena.add(right.clone(), span);
        let result = self
            .dispatch_class_method(Some(id), class, method, &[l, r], span)
            .await?;

        // Post-process for operators that transform the class method result.
        // User `compare` returns `Ordering` (Tagged discriminant: `0`=Lt, `1`=Eq, `2`=Gt).
        match op {
            BinOp::Ne => match result {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                _ => typechecked!("!=", "Bool"),
            },
            BinOp::Lt => match result {
                Value::Tagged(_, d, _) => Ok(Value::Bool(d == 0)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Gt => match result {
                Value::Tagged(_, d, _) => Ok(Value::Bool(d == 2)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Le => match result {
                Value::Tagged(_, d, _) => Ok(Value::Bool(d <= 1)),
                _ => typechecked!("compare result", "Ordering"),
            },
            BinOp::Ge => match result {
                Value::Tagged(_, d, _) => Ok(Value::Bool(d >= 1)),
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
        v: Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            UnOp::Neg => match &v {
                // Fast-path: Int negation (most common)
                Value::Int(n) => Ok(Value::Int(-n)),
                _ => self.dispatch_unary(
                    BuiltinClassTag::Negatable,
                    "neg",
                    &v,
                    span,
                ),
            },
            UnOp::Not => Ok(match &v {
                Value::Bool(b) => Value::Bool(!b),
                _ => typechecked!("NOT", "Bool"),
            }),
            UnOp::Wrap => {
                // Look up target type (defaulted to `Option[T]` during constraint solving)
                let ty_id =
                    self.wrap_types.get(&id).copied().unwrap_or_else(|| {
                        typechecked!("?", "resolved wrap type")
                    });
                let ty = self.ty_arena.get(ty_id).clone();
                self.dispatch_convert(
                    BuiltinClassTag::Fallible,
                    "wrap",
                    &v,
                    &ty,
                    span,
                )
            }
        }
    }

    /// Dispatch a unary class method.
    ///
    /// Unwraps Union/Newtype values before dispatching.
    pub(super) fn dispatch_unary(
        &mut self,
        kind: BuiltinClassTag,
        method: &str,
        v: &Value,
        span: Span,
    ) -> Result<Value> {
        // Unwrap Union/Newtype to auto-derive class methods
        let unwrapped = self.unwrap_value_recursive(v);
        let val = unwrapped.as_ref().unwrap_or(v);

        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_unary(kind, method, &mut ctx, val)
    }

    /// Dispatch a conversion class method (`Into:into`, `TryInto:try-into`).
    ///
    /// Unwraps Union/Newtype values before dispatching.
    pub(super) fn dispatch_convert(
        &mut self,
        kind: BuiltinClassTag,
        method: &str,
        v: &Value,
        target: &crate::typecheck::Ty,
        span: Span,
    ) -> Result<Value> {
        // Unwrap Union/Newtype to auto-derive conversions
        let unwrapped = self.unwrap_value_recursive(v);
        let val = unwrapped.as_ref().unwrap_or(v);

        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        self.class_methods
            .dispatch_convert(kind, method, &mut ctx, val, target)
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
        let ord = self.dispatch_binary(
            BuiltinClassTag::Ord,
            "compare",
            left,
            right,
            span,
        )?;
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
        // Unwrap Union/Newtype to auto-derive
        let unwrapped_l = self.unwrap_value_recursive(left);
        let l = unwrapped_l.as_ref().unwrap_or(left);
        let unwrapped_r = self.unwrap_value_recursive(right);
        let r = unwrapped_r.as_ref().unwrap_or(right);

        match (l, r) {
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
