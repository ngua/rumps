//! Binary and unary operator implementations.

use std::cmp::Ordering;

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{BinOp, UnOp};
use crate::io::IoContext;
use crate::value::{StringId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Get type name for error messages.
    fn type_name(&self, v: &Value) -> &'static str {
        v.type_name(&self.registry, &self.type_exprs)
    }

    /// Apply a binary operation to two values.
    pub(super) fn apply_binop(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            BinOp::Add => self.binop_add(left, right, span),
            BinOp::Sub => self.binop_sub(left, right, span),
            BinOp::Mul => self.binop_mul(left, right, span),
            BinOp::Div => self.binop_div(left, right, span),
            BinOp::FloorDiv => self.binop_floor_div(left, right, span),
            BinOp::Mod => self.binop_mod(left, right, span),
            BinOp::Pow => self.binop_pow(left, right, span),
            BinOp::Eq => self.values_equal(left, right, span).map(Value::Bool),
            BinOp::Ne => self
                .values_equal(left, right, span)
                .map(|eq| Value::Bool(!eq)),
            BinOp::Lt => {
                self.binop_cmp(left, right, span, |o| o == Ordering::Less)
            }
            BinOp::Gt => {
                self.binop_cmp(left, right, span, |o| o == Ordering::Greater)
            }
            BinOp::Le => {
                self.binop_cmp(left, right, span, |o| o != Ordering::Greater)
            }
            BinOp::Ge => {
                self.binop_cmp(left, right, span, |o| o != Ordering::Less)
            }
            BinOp::And | BinOp::Or | BinOp::Coalesce => {
                unreachable!("handled in binary")
            }
            BinOp::Concat => Ok(self.binop_concat(left, right)),
        }
    }

    /// Unary operation application.
    pub(super) fn apply_unop(
        &self,
        op: UnOp,
        v: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            UnOp::Neg => match v {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(OrderedFloat(-f.0))),
                _ => Err(Error::type_err(
                    span,
                    format!("cannot negate {}", self.type_name(v)),
                )),
            },
            UnOp::Not => match v {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                _ => Err(Error::type_err(
                    span,
                    format!(
                        "logical NOT requires Bool; got {}",
                        self.type_name(v)
                    ),
                )),
            },
        }
    }

    /// String concatenation.
    pub(super) fn binop_concat(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Value {
        let result = match (left, right) {
            (Value::String(l), Value::String(r)) => {
                let ls = self.arena.get_str(*l).unwrap_or("");
                let rs = self.arena.get_str(*r).unwrap_or("");
                format!("{ls}{rs}")
            }
            _ => format!("{}{}", self.stringify(left), self.stringify(right)),
        };
        Value::String(self.arena.intern(&result))
    }

    /// Addition with numeric coercion.
    fn binop_add(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                Ok(Value::Int(a.wrapping_add(*b)))
            }
            (Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 + b.0)))
            }
            (Value::Int(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(*a as f64 + b.0)))
            }
            (Value::Float(a), Value::Int(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 + *b as f64)))
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot add {} and {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Subtraction with numeric coercion.
    fn binop_sub(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                Ok(Value::Int(a.wrapping_sub(*b)))
            }
            (Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 - b.0)))
            }
            (Value::Int(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(*a as f64 - b.0)))
            }
            (Value::Float(a), Value::Int(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 - *b as f64)))
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot subtract {} from {}",
                    self.type_name(right),
                    self.type_name(left)
                ),
            )),
        }
    }

    /// Multiplication with numeric coercion.
    fn binop_mul(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                Ok(Value::Int(a.wrapping_mul(*b)))
            }
            (Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 * b.0)))
            }
            (Value::Int(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(*a as f64 * b.0)))
            }
            (Value::Float(a), Value::Int(b)) => {
                Ok(Value::Float(OrderedFloat(a.0 * *b as f64)))
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot multiply {} and {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Division (always returns float).
    fn binop_div(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        let (a, b) = match (left, right) {
            (Value::Int(a), Value::Int(b)) => (*a as f64, *b as f64),
            (Value::Float(a), Value::Float(b)) => (a.0, b.0),
            (Value::Int(a), Value::Float(b)) => (*a as f64, b.0),
            (Value::Float(a), Value::Int(b)) => (a.0, *b as f64),
            _ => {
                return Err(Error::type_err(
                    span,
                    format!(
                        "cannot divide {} by {}",
                        self.type_name(left),
                        self.type_name(right)
                    ),
                ))
            }
        };
        if b == 0.0 {
            Err(Error::runtime(span, "division by zero"))
        } else {
            Ok(Value::Float(OrderedFloat(a / b)))
        }
    }

    /// Floor division (integer division).
    fn binop_floor_div(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int(a.div_euclid(*b)))
                }
            }
            (Value::Float(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((a.0 / b.0).floor() as i64))
                }
            }
            (Value::Int(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((*a as f64 / b.0).floor() as i64))
                }
            }
            (Value::Float(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((a.0 / *b as f64).floor() as i64))
                }
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot floor divide {} by {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Modulo operation.
    fn binop_mod(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Int(a.rem_euclid(*b)))
                }
            }
            (Value::Float(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(a.0 % b.0)))
                }
            }
            (Value::Int(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(*a as f64 % b.0)))
                }
            }
            (Value::Float(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(a.0 % *b as f64)))
                }
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot compute {} mod {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Power/exponentiation.
    fn binop_pow(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match (left, right) {
            // Int ** Int: use checked_pow with u32 exponent
            (Value::Int(base), Value::Int(exp)) => {
                if *exp < 0 {
                    // Negative exponent: convert to float
                    Ok(Value::Float(OrderedFloat(
                        (*base as f64).powf(*exp as f64),
                    )))
                } else {
                    // Non-negative exponent: try integer power
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| base.checked_pow(e))
                        .map_or_else(
                            || {
                                // Overflow: fall back to float
                                Ok(Value::Float(OrderedFloat(
                                    (*base as f64).powf(*exp as f64),
                                )))
                            },
                            |r| Ok(Value::Int(r)),
                        )
                }
            }
            // Float ** Float
            (Value::Float(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat(a.0.powf(b.0))))
            }
            // Mixed: coerce to float
            (Value::Int(a), Value::Float(b)) => {
                Ok(Value::Float(OrderedFloat((*a as f64).powf(b.0))))
            }
            (Value::Float(a), Value::Int(b)) => {
                Ok(Value::Float(OrderedFloat(a.0.powf(*b as f64))))
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot raise {} to power {}",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Compare two values and apply a predicate to the ordering.
    fn binop_cmp<F>(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
        pred: F,
    ) -> Result<Value>
    where
        F: FnOnce(Ordering) -> bool,
    {
        let ord = match (left, right) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.cmp(b),
            (Value::Int(a), Value::Float(b)) => OrderedFloat(*a as f64).cmp(b),
            (Value::Float(a), Value::Int(b)) => a.cmp(&OrderedFloat(*b as f64)),
            (Value::String(a), Value::String(b)) => {
                let sa = self.arena.get_str(*a).unwrap_or("");
                let sb = self.arena.get_str(*b).unwrap_or("");
                sa.cmp(sb)
            }
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            _ => {
                return Err(Error::type_err(
                    span,
                    format!(
                        "cannot compare {} and {}",
                        self.type_name(left),
                        self.type_name(right)
                    ),
                ))
            }
        };
        Ok(Value::Bool(pred(ord)))
    }

    /// Check equality of two values.
    fn values_equal(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<bool> {
        match (left, right) {
            (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
            (Value::Int(a), Value::Int(b)) => Ok(a == b),
            (Value::Float(a), Value::Float(b)) => Ok(a == b),
            (Value::Int(a), Value::Float(b)) => Ok((*a as f64) == b.0),
            (Value::Float(a), Value::Int(b)) => Ok(a.0 == (*b as f64)),
            (Value::String(a), Value::String(b)) => Ok(a == b),
            (Value::Array(_, a), Value::Array(_, b)) => {
                if a.len() != b.len() {
                    Ok(false)
                } else {
                    self.arrays_equal(a, b, span)
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                if a.len() != b.len() {
                    Ok(false)
                } else {
                    self.objects_equal(a, b, span)
                }
            }
            (Value::Tagged(ty1, idx1, p1), Value::Tagged(ty2, idx2, p2)) => {
                // Use structural type equality, not TypeExprId identity
                let types_eq = self.type_exprs.eq(*ty1, *ty2);
                if !types_eq || idx1 != idx2 || p1.len() != p2.len() {
                    Ok(false)
                } else {
                    self.payloads_equal(p1, p2, span)
                }
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "cannot compare {} and {} for equality",
                    self.type_name(left),
                    self.type_name(right)
                ),
            )),
        }
    }

    /// Check equality of two arrays element-wise.
    fn arrays_equal(
        &self,
        a: &SmallVec<[ValueId; 4]>,
        b: &SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<bool> {
        a.iter().zip(b.iter()).try_fold(true, |acc, (av, bv)| {
            self.arena
                .get(*av)
                .zip(self.arena.get(*bv))
                .map(|(va, vb)| self.values_equal(va, vb, span))
                .unwrap_or(Ok(false))
                .map(|eq| acc && eq)
        })
    }

    /// Check equality of two objects field-wise.
    fn objects_equal(
        &self,
        a: &IndexMap<StringId, ValueId>,
        b: &IndexMap<StringId, ValueId>,
        span: Span,
    ) -> Result<bool> {
        a.iter().try_fold(true, |acc, (k, av)| {
            b.get(k)
                .and_then(|bv| {
                    self.arena
                        .get(*av)
                        .zip(self.arena.get(*bv))
                        .map(|(va, vb)| self.values_equal(va, vb, span))
                })
                .unwrap_or(Ok(false))
                .map(|eq| acc && eq)
        })
    }

    /// Check equality of tagged value payloads.
    fn payloads_equal(
        &self,
        p1: &SmallVec<[ValueId; 4]>,
        p2: &SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<bool> {
        p1.iter().zip(p2.iter()).try_fold(true, |acc, (av, bv)| {
            self.arena
                .get(*av)
                .zip(self.arena.get(*bv))
                .map(|(va, vb)| self.values_equal(va, vb, span))
                .unwrap_or(Ok(false))
                .map(|eq| acc && eq)
        })
    }
}
