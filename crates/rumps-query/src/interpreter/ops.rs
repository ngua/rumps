//! Binary and unary operator implementations.

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{BinOp, UnOp};
use crate::io::IoContext;
use crate::value::{StringId, TypeRegistry, Value, ValueArena, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Apply a binary operation to two values.
    pub(super) fn apply_binop(
        &mut self,
        left: &Value,
        op: BinOp,
        right: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            // Arithmetic
            BinOp::Add => binop_add(left, right, &self.registry, span),
            BinOp::Sub => binop_sub(left, right, &self.registry, span),
            BinOp::Mul => binop_mul(left, right, &self.registry, span),
            BinOp::Div => binop_div(left, right, &self.registry, span),
            BinOp::FloorDiv => {
                binop_floor_div(left, right, &self.registry, span)
            }
            BinOp::Mod => binop_mod(left, right, &self.registry, span),

            // Comparison
            BinOp::Eq => {
                values_equal(left, right, &self.arena, &self.registry, span)
                    .map(Value::Bool)
            }
            BinOp::Ne => {
                values_equal(left, right, &self.arena, &self.registry, span)
                    .map(|eq| Value::Bool(!eq))
            }
            BinOp::Lt => binop_compare(
                left,
                right,
                &self.arena,
                &self.registry,
                span,
                |ord| matches!(ord, std::cmp::Ordering::Less),
            ),
            BinOp::Gt => binop_compare(
                left,
                right,
                &self.arena,
                &self.registry,
                span,
                |ord| matches!(ord, std::cmp::Ordering::Greater),
            ),
            BinOp::Le => binop_compare(
                left,
                right,
                &self.arena,
                &self.registry,
                span,
                |ord| {
                    matches!(
                        ord,
                        std::cmp::Ordering::Less | std::cmp::Ordering::Equal
                    )
                },
            ),
            BinOp::Ge => binop_compare(
                left,
                right,
                &self.arena,
                &self.registry,
                span,
                |ord| {
                    matches!(
                        ord,
                        std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
                    )
                },
            ),

            // AND/OR/Coalesce are handled in `binary` for short-circuit semantics
            BinOp::And | BinOp::Or | BinOp::Coalesce => {
                unreachable!("handled in binary")
            }

            // String concatenation
            BinOp::Concat => Ok(self.binop_concat(left, right)),
        }
    }

    /// Unary operation application.
    pub(super) fn apply_unop(
        &self,
        op: UnOp,
        val: &Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            UnOp::Neg => match val {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(OrderedFloat(-f.0))),
                _ => Err(Error::type_err(
                    span,
                    format!("cannot negate {}", val.type_name(&self.registry)),
                )),
            },
            UnOp::Not => match val {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                _ => Err(Error::type_err(
                    span,
                    format!(
                        "logical NOT requires a boolean; got {}",
                        val.type_name(&self.registry)
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
        // Fast path: both are strings
        let result = match (left, right) {
            (Value::String(l), Value::String(r)) => {
                let ls = self.arena.get_str(*l).unwrap_or("");
                let rs = self.arena.get_str(*r).unwrap_or("");
                format!("{ls}{rs}")
            }
            _ => {
                let l = self.stringify(left);
                let r = self.stringify(right);
                format!("{l}{r}")
            }
        };
        Value::String(self.arena.intern(&result))
    }
}

/// Addition with numeric coercion.
fn binop_add(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
    span: Span,
) -> Result<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        (Value::Float(a), Value::Float(b)) => {
            Ok(Value::Float(OrderedFloat(a.0 + b.0)))
        }
        // Int + Float -> Float
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
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Subtraction with numeric coercion.
fn binop_sub(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
    span: Span,
) -> Result<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_sub(*b))),
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
                right.type_name(reg),
                left.type_name(reg)
            ),
        )),
    }
}

/// Multiplication with numeric coercion.
fn binop_mul(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
    span: Span,
) -> Result<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_mul(*b))),
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
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Division (always returns float).
fn binop_div(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
    span: Span,
) -> Result<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => div_f64(*a as f64, *b as f64, span),
        (Value::Float(a), Value::Float(b)) => div_f64(a.0, b.0, span),
        (Value::Int(a), Value::Float(b)) => div_f64(*a as f64, b.0, span),
        (Value::Float(a), Value::Int(b)) => div_f64(a.0, *b as f64, span),
        _ => Err(Error::type_err(
            span,
            format!(
                "cannot divide {} by {}",
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Floor division (integer division).
fn binop_floor_div(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
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
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Modulo operation.
fn binop_mod(
    left: &Value,
    right: &Value,
    reg: &TypeRegistry,
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
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Check equality of two values.
///
/// Returns `Err` if the types are incompatible for comparison.
fn values_equal(
    left: &Value,
    right: &Value,
    arena: &ValueArena,
    reg: &TypeRegistry,
    span: Span,
) -> Result<bool> {
    match (left, right) {
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        (Value::Int(a), Value::Int(b)) => Ok(a == b),
        (Value::Float(a), Value::Float(b)) => Ok(a == b),
        // Cross-type numeric comparison
        (Value::Int(a), Value::Float(b)) => Ok((*a as f64) == b.0),
        (Value::Float(a), Value::Int(b)) => Ok(a.0 == (*b as f64)),
        (Value::String(a), Value::String(b)) => Ok(a == b),
        // Arrays: structural equality (type is checked implicitly by elements)
        (Value::Array(_, a), Value::Array(_, b)) => {
            if a.len() != b.len() {
                Ok(false)
            } else {
                arrays_equal(a, b, arena, reg, span)
            }
        }
        // Objects: structural equality
        (Value::Object(a), Value::Object(b)) => {
            if a.len() != b.len() {
                Ok(false)
            } else {
                objects_equal(a, b, arena, reg, span)
            }
        }
        // Tagged: same type, variant, and payloads
        (Value::Tagged(ty1, idx1, p1), Value::Tagged(ty2, idx2, p2)) => {
            if ty1 != ty2 || idx1 != idx2 || p1.len() != p2.len() {
                Ok(false)
            } else {
                payloads_equal(p1, p2, arena, reg, span)
            }
        }
        // Incompatible types
        _ => Err(Error::type_err(
            span,
            format!(
                "cannot compare {} and {} for equality",
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
}

/// Check equality of two arrays element-wise.
fn arrays_equal(
    a: &SmallVec<[ValueId; 4]>,
    b: &SmallVec<[ValueId; 4]>,
    arena: &ValueArena,
    reg: &TypeRegistry,
    span: Span,
) -> Result<bool> {
    a.iter().zip(b.iter()).try_fold(true, |acc, (av, bv)| {
        arena
            .get(*av)
            .zip(arena.get(*bv))
            .map(|(va, vb)| values_equal(va, vb, arena, reg, span))
            .unwrap_or(Ok(false))
            .map(|eq| acc && eq)
    })
}

/// Check equality of two objects field-wise.
fn objects_equal(
    a: &IndexMap<StringId, ValueId>,
    b: &IndexMap<StringId, ValueId>,
    arena: &ValueArena,
    reg: &TypeRegistry,
    span: Span,
) -> Result<bool> {
    a.iter().try_fold(true, |acc, (k, av)| {
        b.get(k)
            .and_then(|bv| {
                arena
                    .get(*av)
                    .zip(arena.get(*bv))
                    .map(|(va, vb)| values_equal(va, vb, arena, reg, span))
            })
            .unwrap_or(Ok(false))
            .map(|eq| acc && eq)
    })
}

/// Check equality of tagged value payloads.
fn payloads_equal(
    p1: &SmallVec<[ValueId; 2]>,
    p2: &SmallVec<[ValueId; 2]>,
    arena: &ValueArena,
    reg: &TypeRegistry,
    span: Span,
) -> Result<bool> {
    p1.iter().zip(p2.iter()).try_fold(true, |acc, (av, bv)| {
        arena
            .get(*av)
            .zip(arena.get(*bv))
            .map(|(va, vb)| values_equal(va, vb, arena, reg, span))
            .unwrap_or(Ok(false))
            .map(|eq| acc && eq)
    })
}

/// Compare two values and apply a predicate to the ordering.
fn binop_compare<F>(
    left: &Value,
    right: &Value,
    arena: &ValueArena,
    reg: &TypeRegistry,
    span: Span,
    pred: F,
) -> Result<Value>
where
    F: FnOnce(std::cmp::Ordering) -> bool,
{
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => Ok(a.cmp(b)),
        // Cross-type numeric comparison
        (Value::Int(a), Value::Float(b)) => Ok(OrderedFloat(*a as f64).cmp(b)),
        (Value::Float(a), Value::Int(b)) => Ok(a.cmp(&OrderedFloat(*b as f64))),
        (Value::String(a), Value::String(b)) => {
            // Compare by actual string content
            let sa = arena.get_str(*a).unwrap_or("");
            let sb = arena.get_str(*b).unwrap_or("");
            Ok(sa.cmp(sb))
        }
        (Value::Bool(a), Value::Bool(b)) => Ok(a.cmp(b)),
        _ => Err(Error::type_err(
            span,
            format!(
                "cannot compare {} and {}",
                left.type_name(reg),
                right.type_name(reg)
            ),
        )),
    }
    .map(|ord| Value::Bool(pred(ord)))
}

/// Helper for float division with zero check.
fn div_f64(a: f64, b: f64, span: Span) -> Result<Value> {
    if b == 0.0 {
        Err(Error::runtime(span, "division by zero"))
    } else {
        Ok(Value::Float(OrderedFloat(a / b)))
    }
}
