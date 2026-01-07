//! Binary and unary operator implementations.

use std::cmp::Ordering;

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{BinOp, UnOp};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Apply a binary operation to two values.
    ///
    /// Type checker guarantees operand types match the operator requirements.
    /// Division/modulo by zero remain runtime errors (not type-level).
    pub(super) fn apply_binop(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            BinOp::Add => Ok(self.binop_add(left, right)),
            BinOp::Sub => Ok(self.binop_sub(left, right)),
            BinOp::Mul => Ok(self.binop_mul(left, right)),
            BinOp::Div => self.binop_div(left, right, span),
            BinOp::FloorDiv => self.binop_floor_div(left, right, span),
            BinOp::Mod => self.binop_mod(left, right, span),
            BinOp::Pow => Ok(self.binop_pow(left, right)),
            BinOp::Eq => Ok(Value::Bool(self.values_equal(left, right))),
            BinOp::Ne => Ok(Value::Bool(!self.values_equal(left, right))),
            BinOp::Lt => {
                Ok(self.binop_cmp(left, right, |o| o == Ordering::Less))
            }
            BinOp::Gt => {
                Ok(self.binop_cmp(left, right, |o| o == Ordering::Greater))
            }
            BinOp::Le => {
                Ok(self.binop_cmp(left, right, |o| o != Ordering::Greater))
            }
            BinOp::Ge => {
                Ok(self.binop_cmp(left, right, |o| o != Ordering::Less))
            }
            BinOp::And | BinOp::Or | BinOp::Coalesce | BinOp::Pipe => {
                unreachable!("handled in binary")
            }
            BinOp::Concat => Ok(self.binop_concat(left, right)),
            BinOp::BitAnd => Ok(self.binop_bitand(left, right)),
            BinOp::BitOr => Ok(self.binop_bitor(left, right)),
            BinOp::Shl => Ok(self.binop_shl(left, right)),
            BinOp::Shr => Ok(self.binop_shr(left, right)),
        }
    }

    /// Unary operation application.
    ///
    /// Type checker guarantees:
    /// - `-` is only applied to `Int`, `Word`, or `Float`
    /// - `!` is only applied to `Bool`
    /// - `?` can wrap any value in `Option.Some`
    pub(super) fn apply_unop(
        &mut self,
        op: UnOp,
        v: Value,
        span: Span,
    ) -> Value {
        match op {
            UnOp::Neg => match &v {
                Value::Int(n) => Value::Int(-n),
                // Negating a Word coerces to Int
                Value::Word(n) => Value::Int(-(*n as i64)),
                Value::Float(f) => Value::Float(OrderedFloat(-f.0)),
                _ => typechecked!("-", "Numeric"),
            },
            UnOp::Not => match &v {
                Value::Bool(b) => Value::Bool(!b),
                _ => typechecked!("!", "Bool"),
            },
            UnOp::Wrap => {
                let inner_id = self.arena.add(v, span);
                self.make_some(inner_id)
            }
        }
    }

    /// Monoid concatenation (`++`).
    ///
    /// Type checker guarantees both operands are `Monoid` (String, Array, Map,
    /// Option) and have the same type.
    pub(super) fn binop_concat(
        &mut self,
        left: &Value,
        right: &Value,
    ) -> Value {
        match (left, right) {
            // String concatenation
            (Value::String(l), Value::String(r)) => {
                let ls = self.arena.get_str(*l).unwrap_or("");
                let rs = self.arena.get_str(*r).unwrap_or("");
                let result = format!("{ls}{rs}");
                Value::String(self.arena.intern(&result))
            }

            // Array concatenation
            (Value::Array(ty, l), Value::Array(_, r)) => {
                let mut elems = l.clone();
                elems.extend(r.iter().copied());
                Value::Array(*ty, elems)
            }

            // Map merge (RHS bias: right values win for duplicate keys)
            (Value::Map(k_ty, v_ty, l), Value::Map(_, _, r)) => {
                let mut merged = l.clone();
                merged.extend(r.iter().map(|(k, v)| (k.clone(), *v)));
                Value::Map(*k_ty, *v_ty, merged)
            }

            // Option alternative (`<|>`): first `Some` wins
            (Value::Tagged(ty1, idx1, _), Value::Tagged(ty2, idx2, _))
                if self
                    .type_exprs
                    .base_type(*ty1)
                    .is_some_and(|t| t == TypeId::OPTION)
                    && self
                        .type_exprs
                        .base_type(*ty2)
                        .is_some_and(|t| t == TypeId::OPTION) =>
            {
                // idx `0` = None, idx `1` = Some
                if *idx1 == 1 {
                    left.clone()
                } else if *idx2 == 1 {
                    right.clone()
                } else {
                    left.clone()
                }
            }

            _ => typechecked!("++", "Monoid"),
        }
    }

    /// Addition with numeric coercion.
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// Mixed `Word`/`Int` coerces to `Int`; `Word + Word` stays `Word`.
    fn binop_add(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_add(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_add(*b))
            }
            (Value::Word(a), Value::Int(b)) => {
                Value::Int((*a as i64).wrapping_add(*b))
            }
            (Value::Int(a), Value::Word(b)) => {
                Value::Int(a.wrapping_add(*b as i64))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 + b.0))
            }
            (Value::Int(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 + b.0))
            }
            (Value::Float(a), Value::Int(b)) => {
                Value::Float(OrderedFloat(a.0 + *b as f64))
            }
            (Value::Word(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 + b.0))
            }
            (Value::Float(a), Value::Word(b)) => {
                Value::Float(OrderedFloat(a.0 + *b as f64))
            }
            _ => typechecked!("+", "Numeric"),
        }
    }

    /// Subtraction with numeric coercion.
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// `Word - Word` uses saturating subtraction (wraps to `0`).
    /// Other `Word` combinations coerce to `Int`.
    fn binop_sub(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_sub(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_sub(*b))
            }
            (Value::Word(a), Value::Int(b)) => {
                Value::Int((*a as i64).wrapping_sub(*b))
            }
            (Value::Int(a), Value::Word(b)) => {
                Value::Int(a.wrapping_sub(*b as i64))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 - b.0))
            }
            (Value::Int(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 - b.0))
            }
            (Value::Float(a), Value::Int(b)) => {
                Value::Float(OrderedFloat(a.0 - *b as f64))
            }
            (Value::Word(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 - b.0))
            }
            (Value::Float(a), Value::Word(b)) => {
                Value::Float(OrderedFloat(a.0 - *b as f64))
            }
            _ => typechecked!("-", "Numeric"),
        }
    }

    /// Multiplication with numeric coercion.
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// Mixed `Word`/`Int` coerces to `Int`; `Word * Word` stays `Word`.
    fn binop_mul(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_mul(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_mul(*b))
            }
            (Value::Word(a), Value::Int(b)) => {
                Value::Int((*a as i64).wrapping_mul(*b))
            }
            (Value::Int(a), Value::Word(b)) => {
                Value::Int(a.wrapping_mul(*b as i64))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 * b.0))
            }
            (Value::Int(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 * b.0))
            }
            (Value::Float(a), Value::Int(b)) => {
                Value::Float(OrderedFloat(a.0 * *b as f64))
            }
            (Value::Word(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(*a as f64 * b.0))
            }
            (Value::Float(a), Value::Word(b)) => {
                Value::Float(OrderedFloat(a.0 * *b as f64))
            }
            _ => typechecked!("*", "Numeric"),
        }
    }

    /// Division (always returns float).
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// Division by zero remains a runtime error (not type-level).
    fn binop_div(
        &self,
        left: &Value,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        let (a, b) = match (left, right) {
            (Value::Int(a), Value::Int(b)) => (*a as f64, *b as f64),
            (Value::Word(a), Value::Word(b)) => (*a as f64, *b as f64),
            (Value::Word(a), Value::Int(b)) => (*a as f64, *b as f64),
            (Value::Int(a), Value::Word(b)) => (*a as f64, *b as f64),
            (Value::Float(a), Value::Float(b)) => (a.0, b.0),
            (Value::Int(a), Value::Float(b)) => (*a as f64, b.0),
            (Value::Float(a), Value::Int(b)) => (a.0, *b as f64),
            (Value::Word(a), Value::Float(b)) => (*a as f64, b.0),
            (Value::Float(a), Value::Word(b)) => (a.0, *b as f64),
            _ => typechecked!("/", "Numeric"),
        };
        if b == 0.0 {
            Err(Error::runtime(span, "division by zero"))
        } else {
            Ok(Value::Float(OrderedFloat(a / b)))
        }
    }

    /// Floor division (integer division).
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// Division by zero remains a runtime error (not type-level).
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
            (Value::Word(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Word(a / b))
                }
            }
            (Value::Word(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((*a as i64).div_euclid(*b)))
                }
            }
            (Value::Int(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int(a.div_euclid(*b as i64)))
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
            (Value::Word(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((*a as f64 / b.0).floor() as i64))
                }
            }
            (Value::Float(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Value::Int((a.0 / *b as f64).floor() as i64))
                }
            }
            _ => typechecked!("//", "Numeric"),
        }
    }

    /// Modulo operation.
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    /// Modulo by zero remains a runtime error (not type-level).
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
            (Value::Word(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Word(a % b))
                }
            }
            (Value::Word(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Int((*a as i64).rem_euclid(*b)))
                }
            }
            (Value::Int(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Int(a.rem_euclid(*b as i64)))
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
            (Value::Word(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(*a as f64 % b.0)))
                }
            }
            (Value::Float(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(a.0 % *b as f64)))
                }
            }
            _ => typechecked!("%", "Numeric"),
        }
    }

    /// Power/exponentiation.
    ///
    /// Type checker guarantees both operands are `Int`, `Word`, or `Float`.
    fn binop_pow(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            // Int ** Int: use checked_pow with u32 exponent
            (Value::Int(base), Value::Int(exp)) => {
                if *exp < 0 {
                    // Negative exponent: convert to float
                    Value::Float(OrderedFloat((*base as f64).powf(*exp as f64)))
                } else {
                    // Non-negative exponent: try integer power
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| base.checked_pow(e))
                        .map_or_else(
                            || {
                                // Overflow: fall back to float
                                Value::Float(OrderedFloat(
                                    (*base as f64).powf(*exp as f64),
                                ))
                            },
                            Value::Int,
                        )
                }
            }
            // Word ** Word: stay Word, saturate on overflow
            (Value::Word(base), Value::Word(exp)) => Value::Word(
                u32::try_from(*exp)
                    .ok()
                    .and_then(|e| base.checked_pow(e))
                    .unwrap_or(usize::MAX),
            ),
            // Word ** Int
            (Value::Word(base), Value::Int(exp)) => {
                if *exp < 0 {
                    Value::Float(OrderedFloat((*base as f64).powf(*exp as f64)))
                } else {
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| (*base as i64).checked_pow(e))
                        .map_or_else(
                            || {
                                Value::Float(OrderedFloat(
                                    (*base as f64).powf(*exp as f64),
                                ))
                            },
                            Value::Int,
                        )
                }
            }
            // Int ** Word
            (Value::Int(base), Value::Word(exp)) => u32::try_from(*exp)
                .ok()
                .and_then(|e| base.checked_pow(e))
                .map_or_else(
                    || {
                        Value::Float(OrderedFloat(
                            (*base as f64).powf(*exp as f64),
                        ))
                    },
                    Value::Int,
                ),
            // Float ** Float
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0.powf(b.0)))
            }
            // Mixed: coerce to float
            (Value::Int(a), Value::Float(b)) => {
                Value::Float(OrderedFloat((*a as f64).powf(b.0)))
            }
            (Value::Float(a), Value::Int(b)) => {
                Value::Float(OrderedFloat(a.0.powf(*b as f64)))
            }
            (Value::Word(a), Value::Float(b)) => {
                Value::Float(OrderedFloat((*a as f64).powf(b.0)))
            }
            (Value::Float(a), Value::Word(b)) => {
                Value::Float(OrderedFloat(a.0.powf(*b as f64)))
            }
            _ => typechecked!("**", "Numeric"),
        }
    }

    /// Compare two values and apply a predicate to the ordering.
    ///
    /// Type checker guarantees both operands are the same comparable type.
    fn binop_cmp<F>(&self, left: &Value, right: &Value, pred: F) -> Value
    where
        F: FnOnce(Ordering) -> bool,
    {
        let ord = match (left, right) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Word(a), Value::Word(b)) => a.cmp(b),
            (Value::Word(a), Value::Int(b)) => (*a as i64).cmp(b),
            (Value::Int(a), Value::Word(b)) => a.cmp(&(*b as i64)),
            (Value::Float(a), Value::Float(b)) => a.cmp(b),
            (Value::Int(a), Value::Float(b)) => OrderedFloat(*a as f64).cmp(b),
            (Value::Float(a), Value::Int(b)) => a.cmp(&OrderedFloat(*b as f64)),
            (Value::Word(a), Value::Float(b)) => OrderedFloat(*a as f64).cmp(b),
            (Value::Float(a), Value::Word(b)) => {
                a.cmp(&OrderedFloat(*b as f64))
            }
            (Value::String(a), Value::String(b)) => {
                let sa = self.arena.get_str(*a).unwrap_or("");
                let sb = self.arena.get_str(*b).unwrap_or("");
                sa.cmp(sb)
            }
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            _ => typechecked!("</>/<=/>=", "Ord"),
        };
        Value::Bool(pred(ord))
    }

    /// Check equality of two values.
    ///
    /// Type checker guarantees both operands are the same comparable type.
    fn values_equal(&self, left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Unit, Value::Unit) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Word(a), Value::Word(b)) => a == b,
            (Value::Word(a), Value::Int(b)) => (*a as i64) == *b,
            (Value::Int(a), Value::Word(b)) => *a == (*b as i64),
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) => (*a as f64) == b.0,
            (Value::Float(a), Value::Int(b)) => a.0 == (*b as f64),
            (Value::Word(a), Value::Float(b)) => (*a as f64) == b.0,
            (Value::Float(a), Value::Word(b)) => a.0 == (*b as f64),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(_, a), Value::Array(_, b)) => {
                a.len() == b.len() && self.arrays_equal(a, b)
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len() && self.objects_equal(a, b)
            }
            (Value::Tagged(ty1, idx1, p1), Value::Tagged(ty2, idx2, p2)) => {
                // Use structural type equality, not TypeExprId identity
                let types_eq = self.type_exprs.eq(*ty1, *ty2);
                types_eq
                    && idx1 == idx2
                    && p1.len() == p2.len()
                    && self.payloads_equal(p1, p2)
            }
            (Value::Ref(g1, name1, subs1), Value::Ref(g2, name2, subs2)) => {
                g1 == g2
                    && name1 == name2
                    && subs1.len() == subs2.len()
                    && self.payloads_equal(subs1, subs2)
            }
            _ => typechecked!("==/!=", "Eq"),
        }
    }

    /// Check equality of two arrays element-wise.
    fn arrays_equal(
        &self,
        a: &SmallVec<[ValueId; 4]>,
        b: &SmallVec<[ValueId; 4]>,
    ) -> bool {
        a.iter().zip(b.iter()).all(|(av, bv)| {
            self.arena
                .get(*av)
                .zip(self.arena.get(*bv))
                .is_some_and(|(va, vb)| self.values_equal(va, vb))
        })
    }

    /// Check equality of two objects field-wise.
    fn objects_equal(
        &self,
        a: &IndexMap<StringId, ValueId>,
        b: &IndexMap<StringId, ValueId>,
    ) -> bool {
        a.iter().all(|(k, av)| {
            b.get(k)
                .and_then(|bv| {
                    self.arena
                        .get(*av)
                        .zip(self.arena.get(*bv))
                        .map(|(va, vb)| self.values_equal(va, vb))
                })
                .unwrap_or(false)
        })
    }

    /// Check equality of tagged value payloads.
    fn payloads_equal(
        &self,
        p1: &SmallVec<[ValueId; 4]>,
        p2: &SmallVec<[ValueId; 4]>,
    ) -> bool {
        p1.iter().zip(p2.iter()).all(|(av, bv)| {
            self.arena
                .get(*av)
                .zip(self.arena.get(*bv))
                .is_some_and(|(va, vb)| self.values_equal(va, vb))
        })
    }

    /// Bitwise AND.
    ///
    /// Type checker guarantees both operands are `Bool`, `Int`, or `Word`.
    fn binop_bitand(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(*a && *b),
            (Value::Int(a), Value::Int(b)) => Value::Int(a & b),
            (Value::Word(a), Value::Word(b)) => Value::Word(a & b),
            _ => typechecked!("&", "BitLike"),
        }
    }

    /// Bitwise OR.
    ///
    /// Type checker guarantees both operands are `Bool`, `Int`, or `Word`.
    fn binop_bitor(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(*a || *b),
            (Value::Int(a), Value::Int(b)) => Value::Int(a | b),
            (Value::Word(a), Value::Word(b)) => Value::Word(a | b),
            _ => typechecked!("|", "BitLike"),
        }
    }

    /// Shift left.
    ///
    /// Type checker guarantees both operands are `Bool`, `Int`, or `Word`.
    /// For `Bool`, shift left by any amount produces `false` (shifts out the bit).
    fn binop_shl(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Bool(_), Value::Bool(_)) => Value::Bool(false),
            (Value::Int(a), Value::Int(b)) => {
                let shift = (*b as u32) & 63;
                Value::Int(a.wrapping_shl(shift))
            }
            (Value::Word(a), Value::Word(b)) => {
                let shift = (*b as u32) & (usize::BITS - 1);
                Value::Word(a.wrapping_shl(shift))
            }
            _ => typechecked!("<<", "BitLike"),
        }
    }

    /// Shift right.
    ///
    /// Type checker guarantees both operands are `Bool`, `Int`, or `Word`.
    /// For `Bool`, shift right by any amount produces `false` (shifts out the bit).
    /// For `Int`, this is an arithmetic (signed) shift.
    fn binop_shr(&self, left: &Value, right: &Value) -> Value {
        match (left, right) {
            (Value::Bool(_), Value::Bool(_)) => Value::Bool(false),
            (Value::Int(a), Value::Int(b)) => {
                let shift = (*b as u32) & 63;
                Value::Int(a.wrapping_shr(shift))
            }
            (Value::Word(a), Value::Word(b)) => {
                let shift = (*b as u32) & (usize::BITS - 1);
                Value::Word(a.wrapping_shr(shift))
            }
            _ => typechecked!(">>", "BitLike"),
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
