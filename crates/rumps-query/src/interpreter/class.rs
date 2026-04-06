//! Class method dispatch infrastructure.
//!
//! Provides a registry for class methods (like `Numeric:add`, `Fallible:unwrap`)
//! and dispatch functions to invoke them. The dispatch table is indexed by
//! `ClassId` for O(1) lookup.
//!
//! # Organization
//!
//! Each type class is represented by a unit struct implementing `Class`:
//! - `Numeric`: `add`, `sub`, `mul`, `floor-div`, `mod`, `pow`
//! - `Negatable`: `neg`
//! - `BitLike`: `bit-and`, `bit-or`, `shl`, `shr`
//! - `Monoid`: `identity`, `concat`
//! - `Ord`: `compare`
//! - `Eq`: `eq`
//! - `Fallible`: `unwrap`
//! - `Wrappable`: `wrap`
//! - `Chainable`: `chain`
//! - `Indexable`: `index`, `get`
//! - `Mappable`: `map`
//! - `Filterable`: `filter`
//! - `Foldable`: `reduce`
//! - `Iterable`: `length`, `collect`
//!
//! Higher-order class methods use a continuation/trampoline pattern defined in
//! the [`hof`](super::hof) module.
//!
//! [`Interpreter`]: super::Interpreter

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use super::hof::{
    ChainWrapper, Continuation, HofMethodFn, HofState, IterKind, MethodResult,
};
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{Ty, TyArena};
use crate::value::{
    TypeExprArena, TypeExprId, TypeId, TypeRegistry, Value, ValueArena, ValueId,
};
use crate::{ClassId, Error, Result, Span};

/// Context for class method dispatch.
pub(crate) struct ClassCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) type_exprs: &'a mut TypeExprArena,
    pub(crate) ty_arena: &'a TyArena,
    pub(crate) registry: &'a TypeRegistry,
    pub(crate) regex_cache: &'a [regex::Regex],
    pub(crate) span: Span,
}

/// Binary class method signature.
pub(crate) type BinMethodFn =
    fn(&mut ClassCtx<'_>, &Value, &Value) -> Result<Value>;

/// Unary class method signature.
pub(crate) type UnaryMethodFn = fn(&mut ClassCtx<'_>, &Value) -> Result<Value>;

/// Nullary class method signature (e.g., `Monoid::identity`).
///
/// Takes the statically-inferred type to produce the appropriate value.
pub(crate) type NullaryMethodFn = fn(&mut ClassCtx<'_>, &Ty) -> Result<Value>;

/// Conversion method signature (e.g., `Into::into`, `TryInto::try_into`).
///
/// Takes a value and the target type to convert to.
pub(crate) type ConvertMethodFn =
    fn(&mut ClassCtx<'_>, &Value, &Ty) -> Result<Value>;

/// Method dispatch function: binary, unary, nullary, convert, or hof.
#[derive(Clone, Copy)]
pub(crate) enum MethodFn {
    Binary(BinMethodFn),
    Unary(UnaryMethodFn),
    Nullary(NullaryMethodFn),
    Convert(ConvertMethodFn),
    Hof(HofMethodFn),
}

/// Per-class method table.
struct MethodTable {
    methods: HashMap<StringId, MethodFn>,
}

impl MethodTable {
    fn new() -> Self {
        Self {
            methods: HashMap::new(),
        }
    }

    fn register(&mut self, name: StringId, f: MethodFn) {
        self.methods.insert(name, f);
    }

    fn lookup(&self, name: StringId) -> Option<MethodFn> {
        self.methods.get(&name).copied()
    }
}

impl Default for MethodTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry of all class methods, indexed by `ClassId`.
pub(crate) struct ClassMethods {
    tables: Vec<MethodTable>,
}

impl ClassMethods {
    pub(crate) fn new() -> Self {
        Self {
            tables: (0..ClassId::BUILTIN_COUNT)
                .map(|_| MethodTable::new())
                .collect(),
        }
    }

    pub(crate) fn register(
        &mut self,
        kind: ClassId,
        name: StringId,
        f: MethodFn,
    ) {
        self.tables[kind.idx()].register(name, f);
    }

    pub(crate) fn lookup(
        &self,
        kind: ClassId,
        name: StringId,
    ) -> Option<MethodFn> {
        self.tables[kind.idx()].lookup(name)
    }

    pub(crate) fn dispatch_binary(
        &self,
        kind: ClassId,
        method: StringId,
        ctx: &mut ClassCtx<'_>,
        recv: &Value,
        arg: &Value,
    ) -> Result<Value> {
        match self.lookup(kind, method) {
            Some(MethodFn::Binary(f)) => f(ctx, recv, arg),
            Some(
                MethodFn::Unary(_)
                | MethodFn::Nullary(_)
                | MethodFn::Convert(_)
                | MethodFn::Hof(_),
            ) => {
                typechecked!("dispatch_binary", "binary method")
            }
            None => typechecked!("dispatch_binary", "registered method"),
        }
    }

    pub(crate) fn dispatch_unary(
        &self,
        kind: ClassId,
        method: StringId,
        ctx: &mut ClassCtx<'_>,
        recv: &Value,
    ) -> Result<Value> {
        match self.lookup(kind, method) {
            Some(MethodFn::Unary(f)) => f(ctx, recv),
            Some(
                MethodFn::Binary(_)
                | MethodFn::Nullary(_)
                | MethodFn::Convert(_)
                | MethodFn::Hof(_),
            ) => {
                typechecked!("dispatch_unary", "unary method")
            }
            None => typechecked!("dispatch_unary", "registered method"),
        }
    }

    pub(crate) fn dispatch_nullary(
        &self,
        kind: ClassId,
        method: StringId,
        ctx: &mut ClassCtx<'_>,
        ty: &Ty,
    ) -> Result<Value> {
        match self.lookup(kind, method) {
            Some(MethodFn::Nullary(f)) => f(ctx, ty),
            Some(
                MethodFn::Binary(_)
                | MethodFn::Unary(_)
                | MethodFn::Convert(_)
                | MethodFn::Hof(_),
            ) => {
                typechecked!("dispatch_nullary", "nullary method")
            }
            None => typechecked!("dispatch_nullary", "registered method"),
        }
    }

    pub(crate) fn dispatch_convert(
        &self,
        kind: ClassId,
        method: StringId,
        ctx: &mut ClassCtx<'_>,
        val: &Value,
        target: &Ty,
    ) -> Result<Value> {
        match self.lookup(kind, method) {
            Some(MethodFn::Convert(f)) => f(ctx, val, target),
            Some(
                MethodFn::Binary(_)
                | MethodFn::Unary(_)
                | MethodFn::Nullary(_)
                | MethodFn::Hof(_),
            ) => {
                typechecked!("dispatch_convert", "convert method")
            }
            None => typechecked!("dispatch_convert", "registered method"),
        }
    }

    /// Register all class methods.
    pub(crate) fn register_all(&mut self, i: &mut StringInterner) {
        self.register(
            ClassId::NUMERIC,
            i.intern("add"),
            MethodFn::Binary(Numeric::add),
        );
        self.register(
            ClassId::NUMERIC,
            i.intern("sub"),
            MethodFn::Binary(Numeric::sub),
        );
        self.register(
            ClassId::NUMERIC,
            i.intern("mul"),
            MethodFn::Binary(Numeric::mul),
        );
        self.register(
            ClassId::NUMERIC,
            i.intern("floor-div"),
            MethodFn::Binary(Numeric::floor_div),
        );
        self.register(
            ClassId::NUMERIC,
            i.intern("mod"),
            MethodFn::Binary(Numeric::modulo),
        );
        self.register(
            ClassId::NUMERIC,
            i.intern("pow"),
            MethodFn::Binary(Numeric::pow),
        );

        self.register(
            ClassId::NEGATABLE,
            i.intern("neg"),
            MethodFn::Unary(Negatable::neg),
        );

        self.register(
            ClassId::BIT_LIKE,
            i.intern("bit-and"),
            MethodFn::Binary(BitLike::and),
        );
        self.register(
            ClassId::BIT_LIKE,
            i.intern("bit-or"),
            MethodFn::Binary(BitLike::or),
        );
        self.register(
            ClassId::BIT_LIKE,
            i.intern("shl"),
            MethodFn::Binary(BitLike::shl),
        );
        self.register(
            ClassId::BIT_LIKE,
            i.intern("shr"),
            MethodFn::Binary(BitLike::shr),
        );

        self.register(
            ClassId::ORD,
            i.intern("compare"),
            MethodFn::Binary(Ord::compare),
        );

        self.register(ClassId::EQ, i.intern("eq"), MethodFn::Binary(Eq::eq));

        self.register(
            ClassId::MONOID,
            i.intern("concat"),
            MethodFn::Binary(Monoid::concat),
        );
        self.register(
            ClassId::MONOID,
            i.intern("identity"),
            MethodFn::Nullary(Monoid::identity),
        );

        self.register(
            ClassId::FALLIBLE,
            i.intern("unwrap"),
            MethodFn::Unary(Fallible::unwrap),
        );
        self.register(
            ClassId::WRAPPABLE,
            i.intern("wrap"),
            MethodFn::Convert(Wrappable::wrap),
        );

        self.register(
            ClassId::INDEXABLE,
            i.intern("index"),
            MethodFn::Binary(Indexable::index),
        );
        self.register(
            ClassId::INDEXABLE,
            i.intern("get"),
            MethodFn::Binary(Indexable::get),
        );

        self.register(
            ClassId::INTO,
            i.intern("into"),
            MethodFn::Convert(Into::into),
        );

        self.register(
            ClassId::TRY_INTO,
            i.intern("try-into"),
            MethodFn::Convert(TryInto::try_into),
        );

        self.register(
            ClassId::DISPLAY,
            i.intern("display"),
            MethodFn::Unary(Display::display),
        );

        // HoF methods (handled via trampoline in `call.rs`).
        self.register(
            ClassId::MAPPABLE,
            i.intern("map"),
            MethodFn::Hof(Mappable::map),
        );
        self.register(
            ClassId::FILTERABLE,
            i.intern("filter"),
            MethodFn::Hof(Filterable::filter),
        );
        self.register(
            ClassId::FOLDABLE,
            i.intern("reduce"),
            MethodFn::Hof(Foldable::reduce),
        );
        self.register(
            ClassId::ITERABLE,
            i.intern("length"),
            MethodFn::Unary(Iterable::length),
        );
        self.register(
            ClassId::ITERABLE,
            i.intern("collect"),
            MethodFn::Unary(Iterable::collect),
        );
        self.register(
            ClassId::CHAINABLE,
            i.intern("chain"),
            MethodFn::Hof(Chainable::chain),
        );
    }
}

impl Default for ClassMethods {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared utilities for class method implementations.
pub(crate) trait Class {
    fn map_key(v: &Value) -> crate::value::MapKey {
        crate::value::MapKey::from_value(v)
            .unwrap_or_else(|| typechecked!("map key", "valid key type"))
    }
}

/// Arithmetic operations for `Int`, `Word`, `Float`.
pub(crate) struct Numeric;

impl Class for Numeric {}

impl Numeric {
    pub(crate) fn add(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_add(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_add(*b))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 + b.0))
            }
            _ => typechecked!("+", "same Numeric type"),
        })
    }

    pub(crate) fn sub(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_sub(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_sub(*b))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 - b.0))
            }
            _ => typechecked!("-", "same Numeric type"),
        })
    }

    pub(crate) fn mul(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Int(a), Value::Int(b)) => Value::Int(a.wrapping_mul(*b)),
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.saturating_mul(*b))
            }
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0 * b.0))
            }
            _ => typechecked!("*", "same Numeric type"),
        })
    }

    pub(crate) fn floor_div(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Value::Int(a.div_euclid(*b)))
                }
            }
            (Value::Word(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Value::Word(a / b))
                }
            }
            (Value::Float(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat((a.0 / b.0).floor())))
                }
            }
            _ => typechecked!("//", "same Numeric type"),
        }
    }

    pub(crate) fn modulo(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Value::Int(a.rem_euclid(*b)))
                }
            }
            (Value::Word(a), Value::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Value::Word(a % b))
                }
            }
            (Value::Float(a), Value::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Value::Float(OrderedFloat(a.0 % b.0)))
                }
            }
            _ => typechecked!("%", "same Numeric type"),
        }
    }

    pub(crate) fn pow(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Int(base), Value::Int(exp)) => {
                if *exp < 0 {
                    Value::Float(OrderedFloat((*base as f64).powf(*exp as f64)))
                } else {
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| base.checked_pow(e))
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
            (Value::Word(base), Value::Word(exp)) => Value::Word(
                u32::try_from(*exp)
                    .ok()
                    .and_then(|e| base.checked_pow(e))
                    .unwrap_or(usize::MAX),
            ),
            (Value::Float(a), Value::Float(b)) => {
                Value::Float(OrderedFloat(a.0.powf(b.0)))
            }
            _ => typechecked!("**", "same Numeric type"),
        })
    }
}

/// Unary negation for `Int`, `Float`.
pub(crate) struct Negatable;

impl Class for Negatable {}

impl Negatable {
    pub(crate) fn neg(_: &mut ClassCtx<'_>, v: &Value) -> Result<Value> {
        Ok(match v {
            Value::Int(n) => Value::Int(-n),
            Value::Float(f) => Value::Float(OrderedFloat(-f.0)),
            _ => typechecked!("-", "Negatable"),
        })
    }
}

/// Bitwise operations for `Bool`, `Int`, `Word`.
pub(crate) struct BitLike;

impl Class for BitLike {}

impl BitLike {
    pub(crate) fn and(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(*a && *b),
            (Value::Int(a), Value::Int(b)) => Value::Int(a & b),
            (Value::Word(a), Value::Word(b)) => Value::Word(a & b),
            _ => typechecked!("&", "BitLike"),
        })
    }

    pub(crate) fn or(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Bool(a), Value::Bool(b)) => Value::Bool(*a || *b),
            (Value::Int(a), Value::Int(b)) => Value::Int(a | b),
            (Value::Word(a), Value::Word(b)) => Value::Word(a | b),
            _ => typechecked!("|", "BitLike"),
        })
    }

    pub(crate) fn shl(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Bool(_), Value::Bool(_)) => Value::Bool(false),
            (Value::Int(a), Value::Int(b)) => {
                Value::Int(a.wrapping_shl((*b as u32) & 63))
            }
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.wrapping_shl((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!("<<", "BitLike"),
        })
    }

    pub(crate) fn shr(
        _: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::Bool(_), Value::Bool(_)) => Value::Bool(false),
            (Value::Int(a), Value::Int(b)) => {
                Value::Int(a.wrapping_shr((*b as u32) & 63))
            }
            (Value::Word(a), Value::Word(b)) => {
                Value::Word(a.wrapping_shr((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!(">>", "BitLike"),
        })
    }
}

/// Concatenation and identity for `String`, `Array`, `Map`, `Option`.
pub(crate) struct Monoid;

impl Class for Monoid {}

impl Monoid {
    /// The monoid identity element for a given type.
    ///
    /// - `String` -> `""`
    /// - `Array[T]` -> `[]`
    /// - `Map[K, V]` -> `{}`
    /// - `Option[T]` -> `Option.None`
    ///
    /// For container types, the element/key/value types use `intern_ty_lenient`
    /// which gracefully handles unresolved type variables (from generics) by
    /// substituting `UNKNOWN`. This is safe because empty containers don't
    /// contain any values that need type checking.
    pub(crate) fn identity(ctx: &mut ClassCtx<'_>, ty: &Ty) -> Result<Value> {
        Ok(match ty {
            Ty::String => Value::String(ctx.arena.intern("")),
            Ty::Array(elem) => {
                let elem_ty =
                    ctx.type_exprs.intern_ty_lenient(*elem, ctx.ty_arena);
                Value::Array(elem_ty, Arc::new(SmallVec::new()))
            }
            Ty::Map(k, v) => {
                let k_ty = ctx.type_exprs.intern_ty_lenient(*k, ctx.ty_arena);
                let v_ty = ctx.type_exprs.intern_ty_lenient(*v, ctx.ty_arena);
                Value::Map(k_ty, v_ty, Arc::new(IndexMap::new()))
            }
            Ty::Option(inner) => {
                let inner_ty =
                    ctx.type_exprs.intern_ty_lenient(*inner, ctx.ty_arena);
                let opt_ty =
                    ctx.type_exprs.app(TypeId::OPTION, smallvec![inner_ty]);
                Value::none(opt_ty)
            }
            _ => typechecked!("identity", "Monoid type"),
        })
    }

    pub(crate) fn concat(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(match (l, r) {
            (Value::String(ls), Value::String(rs)) => {
                let l = ctx.arena.get_str(*ls).unwrap_or("");
                let r = ctx.arena.get_str(*rs).unwrap_or("");
                Value::String(ctx.arena.intern(&format!("{l}{r}")))
            }
            (Value::Array(ty, l), Value::Array(_, r)) => {
                let mut elems = Arc::unwrap_or_clone(l.clone());
                elems.extend(r.iter().copied());
                Value::Array(*ty, Arc::new(elems))
            }
            (Value::Map(k_ty, v_ty, l), Value::Map(_, _, r)) => {
                let mut merged = Arc::unwrap_or_clone(l.clone());
                merged.extend(r.iter().map(|(k, v)| (k.clone(), *v)));
                Value::Map(*k_ty, *v_ty, Arc::new(merged))
            }
            (Value::Tagged(ty1, i1, p1), Value::Tagged(ty2, i2, p2))
                if ctx
                    .type_exprs
                    .base_type(*ty1)
                    .is_some_and(|t| t == TypeId::OPTION)
                    && ctx
                        .type_exprs
                        .base_type(*ty2)
                        .is_some_and(|t| t == TypeId::OPTION) =>
            {
                if *i1 == 1 {
                    Value::Tagged(*ty1, *i1, p1.clone())
                } else if *i2 == 1 {
                    Value::Tagged(*ty2, *i2, p2.clone())
                } else {
                    Value::Tagged(*ty1, *i1, SmallVec::new())
                }
            }
            _ => typechecked!("++", "Monoid"),
        })
    }
}

/// Comparison returning `-1`, `0`, or `1`.
pub(crate) struct Ord;

impl Class for Ord {}

impl Ord {
    pub(crate) fn compare(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        use std::cmp::Ordering;
        let ord = Self::cmp_values(ctx, l, r);
        Ok(Value::Int(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    }

    /// Recursive comparison helper returning `std::cmp::Ordering`.
    fn cmp_values(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Word(a), Value::Word(b)) => a.cmp(b),
            (Value::Float(a), Value::Float(b)) => a.cmp(b),
            (Value::String(a), Value::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa.cmp(sb)
            }
            (Value::Char(a), Value::Char(b)) => a.cmp(b),
            (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
            (Value::Time(a), Value::Time(b)) => a.cmp(b),
            // Arrays: lexicographic comparison
            (Value::Array(_, a), Value::Array(_, b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            // Tuples: lexicographic comparison
            (Value::Tuple(_, a), Value::Tuple(_, b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            // Maps: lexicographic comparison by (key, value) pairs sorted by key
            (Value::Map(_, _, a), Value::Map(_, _, b)) => {
                Self::cmp_maps(ctx, a, b)
            }
            // Tagged (Option, Result, user types): compare variant index, then payload
            // Note: Result has Ok=0, Err=1, but we want Err < Ok, so reverse for Result
            (Value::Tagged(ty, i1, p1), Value::Tagged(_, i2, p2)) => {
                let is_result = ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT);
                let idx_ord = if is_result { i2.cmp(i1) } else { i1.cmp(i2) };
                match idx_ord {
                    Ordering::Equal => {
                        Self::cmp_seqs(ctx, p1.as_slice(), p2.as_slice())
                    }
                    ord => ord,
                }
            }
            // Union: types must match, then compare inner values if same type
            (Value::Union(ty1, id1), Value::Union(ty2, id2)) => {
                if !ctx.type_exprs.eq(*ty1, *ty2) {
                    typechecked!("compare", "same Ord type")
                }
                ctx.arena
                    .get(*id1)
                    .cloned()
                    .zip(ctx.arena.get(*id2).cloned())
                    .map(|(a, b)| {
                        if std::mem::discriminant(&a)
                            == std::mem::discriminant(&b)
                        {
                            Self::cmp_values(ctx, &a, &b)
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .unwrap_or_else(|| {
                        invariant!("Union inner value missing from arena")
                    })
            }
            // Newtype: types must match, then compare inner values
            (Value::Newtype(ty1, id1), Value::Newtype(ty2, id2)) => {
                if !ctx.type_exprs.eq(*ty1, *ty2) {
                    typechecked!("compare", "same Ord type")
                }
                ctx.arena
                    .get(*id1)
                    .cloned()
                    .zip(ctx.arena.get(*id2).cloned())
                    .map(|(a, b)| Self::cmp_values(ctx, &a, &b))
                    .unwrap_or_else(|| {
                        invariant!("Newtype inner value missing from arena")
                    })
            }
            _ => typechecked!("compare", "same Ord type"),
        }
    }

    /// Lexicographic comparison of sequences of `ValueId`s.
    fn cmp_seqs(
        ctx: &mut ClassCtx<'_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> std::cmp::Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(ai, bi)| {
                let av = ctx.arena.get(*ai).cloned();
                let bv = ctx.arena.get(*bi).cloned();
                match (av, bv) {
                    (Some(av), Some(bv)) => Self::cmp_values(ctx, &av, &bv),
                    _ => std::cmp::Ordering::Equal,
                }
            })
            .find(|o| *o != std::cmp::Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    }

    /// Compare two maps by sorting entries by key, then comparing lexicographically.
    fn cmp_maps(
        ctx: &mut ClassCtx<'_>,
        a: &IndexMap<crate::value::MapKey, ValueId>,
        b: &IndexMap<crate::value::MapKey, ValueId>,
    ) -> std::cmp::Ordering {
        // Collect and sort entries by key (convert MapKey to Value for comparison)
        let mut a_entries: Vec<_> =
            a.iter().map(|(k, v)| (k.to_value(), *v)).collect();
        let mut b_entries: Vec<_> =
            b.iter().map(|(k, v)| (k.to_value(), *v)).collect();
        a_entries.sort_by(|(k1, _), (k2, _)| Self::cmp_values(ctx, k1, k2));
        b_entries.sort_by(|(k1, _), (k2, _)| Self::cmp_values(ctx, k1, k2));
        // Compare lexicographically by (key, value) pairs
        a_entries
            .iter()
            .zip(b_entries.iter())
            .map(|((k1, v1), (k2, v2))| {
                let key_ord = Self::cmp_values(ctx, k1, k2);
                if key_ord != std::cmp::Ordering::Equal {
                    key_ord
                } else {
                    let v1 = ctx.arena.get(*v1).cloned();
                    let v2 = ctx.arena.get(*v2).cloned();
                    match (v1, v2) {
                        (Some(v1), Some(v2)) => Self::cmp_values(ctx, &v1, &v2),
                        _ => std::cmp::Ordering::Equal,
                    }
                }
            })
            .find(|o| *o != std::cmp::Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    }
}

/// Equality comparison.
pub(crate) struct Eq;

impl Class for Eq {}

impl Eq {
    pub(crate) fn eq(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Value> {
        Ok(Value::Bool(Self::values_equal(ctx, l, r)))
    }

    /// Recursive equality helper.
    fn values_equal(ctx: &mut ClassCtx<'_>, l: &Value, r: &Value) -> bool {
        match (l, r) {
            (Value::Unit, Value::Unit) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Word(a), Value::Word(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::String(a), Value::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Value::Time(a), Value::Time(b)) => a == b,
            (Value::FilePath(a), Value::FilePath(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Value::Json(a), Value::Json(b)) => a == b,
            (Value::Array(_, a), Value::Array(_, b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Value::Tuple(_, a), Value::Tuple(_, b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len() && Self::objects_equal(ctx, a, b)
            }
            (Value::Map(_, _, a), Value::Map(_, _, b)) => {
                a.len() == b.len() && Self::maps_equal(ctx, a, b)
            }
            (Value::Tagged(ty1, idx1, p1), Value::Tagged(ty2, idx2, p2)) => {
                let types_eq = ctx.type_exprs.eq(*ty1, *ty2);
                types_eq
                    && idx1 == idx2
                    && p1.len() == p2.len()
                    && Self::seqs_equal(ctx, p1.as_slice(), p2.as_slice())
            }
            (Value::Ref(g1, name1, subs1), Value::Ref(g2, name2, subs2)) => {
                g1 == g2
                    && name1 == name2
                    && subs1.len() == subs2.len()
                    && Self::seqs_equal(ctx, subs1.as_slice(), subs2.as_slice())
            }
            // Union: types must match, inner types must match, then values must match
            (Value::Union(ty1, id1), Value::Union(ty2, id2)) => {
                ctx.type_exprs.eq(*ty1, *ty2)
                    && ctx
                        .arena
                        .get(*id1)
                        .cloned()
                        .zip(ctx.arena.get(*id2).cloned())
                        .is_some_and(|(a, b)| {
                            std::mem::discriminant(&a)
                                == std::mem::discriminant(&b)
                                && Self::values_equal(ctx, &a, &b)
                        })
            }
            // Newtype: types must match, then compare inner values
            (Value::Newtype(ty1, id1), Value::Newtype(ty2, id2)) => {
                ctx.type_exprs.eq(*ty1, *ty2)
                    && ctx
                        .arena
                        .get(*id1)
                        .cloned()
                        .zip(ctx.arena.get(*id2).cloned())
                        .is_some_and(|(a, b)| Self::values_equal(ctx, &a, &b))
            }
            _ => typechecked!("==", "same Eq type"),
        }
    }

    /// Element-wise equality for sequences (arrays, tuples, payloads).
    fn seqs_equal(
        ctx: &mut ClassCtx<'_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> bool {
        a.iter().zip(b.iter()).all(|(ai, bi)| {
            let av = ctx.arena.get(*ai).cloned();
            let bv = ctx.arena.get(*bi).cloned();
            match (av, bv) {
                (Some(av), Some(bv)) => Self::values_equal(ctx, &av, &bv),
                _ => false,
            }
        })
    }

    /// Field-wise equality for objects.
    fn objects_equal(
        ctx: &mut ClassCtx<'_>,
        a: &IndexMap<StringId, ValueId>,
        b: &IndexMap<StringId, ValueId>,
    ) -> bool {
        a.iter().all(|(k, av)| {
            b.get(k)
                .and_then(|bv| {
                    let av_clone = ctx.arena.get(*av).cloned();
                    let bv_clone = ctx.arena.get(*bv).cloned();
                    match (av_clone, bv_clone) {
                        (Some(av), Some(bv)) => {
                            Some(Self::values_equal(ctx, &av, &bv))
                        }
                        _ => None,
                    }
                })
                .unwrap_or(false)
        })
    }

    /// Equality for maps (order-independent, compare entries).
    fn maps_equal(
        ctx: &mut ClassCtx<'_>,
        a: &IndexMap<crate::value::MapKey, ValueId>,
        b: &IndexMap<crate::value::MapKey, ValueId>,
    ) -> bool {
        if !a.keys().all(|k| b.contains_key(k)) {
            false
        } else {
            a.iter().all(|(k, av)| {
                b.get(k)
                    .and_then(|bv| {
                        let av_clone = ctx.arena.get(*av).cloned();
                        let bv_clone = ctx.arena.get(*bv).cloned();
                        match (av_clone, bv_clone) {
                            (Some(av), Some(bv)) => {
                                Some(Self::values_equal(ctx, &av, &bv))
                            }
                            _ => None,
                        }
                    })
                    .unwrap_or(false)
            })
        }
    }
}

/// Unwrap for `Option` and `Result`.
pub(crate) struct Fallible;

impl Class for Fallible {}

impl Fallible {
    /// Unwrap an `Option.Some` or `Result.Ok` value.
    ///
    /// Wraps the inner value if the type parameter is a union/newtype.
    pub(crate) fn unwrap(ctx: &mut ClassCtx<'_>, v: &Value) -> Result<Value> {
        let is_opt = |ty: TypeExprId| {
            ctx.type_exprs
                .base_type(ty)
                .is_some_and(|t| t == TypeId::OPTION)
        };
        let is_res = |ty: TypeExprId| {
            ctx.type_exprs
                .base_type(ty)
                .is_some_and(|t| t == TypeId::RESULT)
        };

        match v {
            Value::Tagged(ty, 1, p) if is_opt(*ty) => {
                // Get inner type from Option[T]
                let inner_ty = ctx
                    .type_exprs
                    .type_args(*ty)
                    .and_then(|args| args.first().copied());
                let val = p
                    .first()
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("unwrap", "Option.Some payload")
                    });
                Ok(match inner_ty {
                    Some(ity) => Self::wrap_for_type(ctx, val, ity),
                    None => val,
                })
            }
            Value::Tagged(ty, 0, _) if is_opt(*ty) => {
                Err(Error::runtime(ctx.span, "cannot unwrap Option.None"))
            }
            Value::Tagged(ty, 0, p) if is_res(*ty) => {
                // Get ok type from Result[Ok, Err]
                let ok_ty = ctx
                    .type_exprs
                    .type_args(*ty)
                    .and_then(|args| args.first().copied());
                let val = p
                    .first()
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("unwrap", "Result.Ok payload")
                    });
                Ok(match ok_ty {
                    Some(oty) => Self::wrap_for_type(ctx, val, oty),
                    None => val,
                })
            }
            Value::Tagged(ty, 1, _) if is_res(*ty) => {
                Err(Error::runtime(ctx.span, "cannot unwrap Result.Err"))
            }
            _ => typechecked!("unwrap", "Fallible"),
        }
    }

    /// Wrap a value in `Value::Union` or `Value::Newtype` if the type requires it.
    fn wrap_for_type(
        ctx: &mut ClassCtx<'_>,
        val: Value,
        ty: TypeExprId,
    ) -> Value {
        let already_wrapped = match &val {
            Value::Union(t, _) => ctx.type_exprs.eq(*t, ty),
            Value::Newtype(t, _) => ctx.type_exprs.eq(*t, ty),
            _ => false,
        };

        if already_wrapped {
            val
        } else {
            ctx.type_exprs
                .base_type(ty)
                .and_then(|type_id| {
                    ctx.registry.get_def(type_id).and_then(|def| match def {
                        crate::value::TypeDef::Union { .. } => {
                            let inner_id = ctx.arena.add(val.clone(), ctx.span);
                            Some(Value::Union(ty, inner_id))
                        }
                        crate::value::TypeDef::Alias { .. } => {
                            let inner_id = ctx.arena.add(val.clone(), ctx.span);
                            Some(Value::Newtype(ty, inner_id))
                        }
                        _ => None,
                    })
                })
                .or_else(|| {
                    if ctx.type_exprs.is_union(ty) {
                        let inner_id = ctx.arena.add(val.clone(), ctx.span);
                        Some(Value::Union(ty, inner_id))
                    } else {
                        None
                    }
                })
                .unwrap_or(val)
        }
    }
}

/// Wrap a value into a fallible container (`Option.Some` or `Result.Ok`).
pub(crate) struct Wrappable;

impl Class for Wrappable {}

impl Wrappable {
    /// Wrap a value in a `Wrappable` container (`Option.Some` or `Result.Ok`).
    ///
    /// The target type determines whether to produce:
    /// - `Option[T]` -> `Option.Some(v)`
    /// - `Result[T, E]` -> `Result.Ok(v)`
    pub(crate) fn wrap(
        ctx: &mut ClassCtx<'_>,
        v: &Value,
        target: &Ty,
    ) -> Result<Value> {
        let v_id = ctx.arena.add(v.clone(), ctx.span);
        match target {
            Ty::Option(inner) => {
                let inner_ty =
                    ctx.type_exprs.intern_ty_lenient(*inner, ctx.ty_arena);
                let opt_ty =
                    ctx.type_exprs.app(TypeId::OPTION, smallvec![inner_ty]);
                Ok(Value::some(opt_ty, v_id))
            }
            Ty::Result(ok, err) => {
                let ok_ty = ctx.type_exprs.intern_ty_lenient(*ok, ctx.ty_arena);
                let err_ty =
                    ctx.type_exprs.intern_ty_lenient(*err, ctx.ty_arena);
                let res_ty = ctx
                    .type_exprs
                    .app(TypeId::RESULT, smallvec![ok_ty, err_ty]);
                Ok(Value::ok(res_ty, v_id))
            }
            _ => typechecked!("wrap", "Wrappable (Option or Result)"),
        }
    }
}

/// Monadic chaining for `Option` and `Result`.
pub(crate) struct Chainable;

impl Class for Chainable {}

/// Indexing for `Array`, `Map`, `String`.
pub(crate) struct Indexable;

impl Class for Indexable {}

impl Indexable {
    pub(crate) fn index(
        ctx: &mut ClassCtx<'_>,
        base: &Value,
        idx: &Value,
    ) -> Result<Value> {
        match (base, idx) {
            (Value::Array(_, elems), Value::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            ctx.span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Value::Map(_, _, entries), key) => entries
                .get(&Self::map_key(key))
                .and_then(|id| ctx.arena.get(*id).cloned())
                .ok_or_else(|| {
                    Error::runtime(
                        ctx.span,
                        format!("map key not found: {key:?}"),
                    )
                }),
            (Value::String(sid), Value::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars().nth(index as usize).map(Value::Char).ok_or_else(
                    || {
                        Error::runtime(
                            ctx.span,
                            format!("string index {i} out of bounds"),
                        )
                    },
                )
            }
            _ => typechecked!("index", "Indexable"),
        }
    }

    pub(crate) fn get(
        ctx: &mut ClassCtx<'_>,
        base: &Value,
        idx: &Value,
    ) -> Result<Value> {
        use smallvec::smallvec;
        match (base, idx) {
            (Value::Array(elem_ty, elems), Value::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                let opt_ty =
                    ctx.type_exprs.app(TypeId::OPTION, smallvec![*elem_ty]);
                Ok(index
                    .and_then(|idx| elems.get(idx))
                    .map(|id| Value::some(opt_ty, *id))
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            (Value::Map(_, v_ty, entries), key) => {
                let opt_ty =
                    ctx.type_exprs.app(TypeId::OPTION, smallvec![*v_ty]);
                Ok(entries
                    .get(&Self::map_key(key))
                    .map(|id| Value::some(opt_ty, *id))
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            (Value::String(sid), Value::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                let char_ty = ctx.type_exprs.named(TypeId::CHAR);
                let opt_ty =
                    ctx.type_exprs.app(TypeId::OPTION, smallvec![char_ty]);
                Ok(s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        let id = ctx.arena.add(Value::Char(c), ctx.span);
                        Value::some(opt_ty, id)
                    })
                    .unwrap_or_else(|| Value::none(opt_ty)))
            }
            _ => typechecked!("get", "Indexable"),
        }
    }
}

/// Infallible conversion: `T: Into[U]` means `T` can be converted to `U`.
pub(crate) struct Into;

impl Class for Into {}

impl Into {
    /// Convert a value to the target type.
    ///
    /// Handles all conversions supported by `AS`:
    /// - Numeric widening (`Int -> Float`, `Word -> Int`, etc.)
    /// - `T -> String` (stringify)
    /// - `T -> Json` (jsonify)
    /// - `String -> FilePath`
    /// - `Path -> FilePath`
    /// - `DataStatus -> Int`
    pub(crate) fn into(
        ctx: &mut ClassCtx<'_>,
        val: &Value,
        target: &Ty,
    ) -> Result<Value> {
        match (val, target) {
            // Identity casts
            (Value::Int(_), Ty::Int)
            | (Value::Word(_), Ty::Word)
            | (Value::Float(_), Ty::Float)
            | (Value::Bool(_), Ty::Bool)
            | (Value::Char(_), Ty::Char)
            | (Value::String(_), Ty::String)
            | (Value::FilePath(_), Ty::FilePath) => Ok(val.clone()),

            // Int -> Float (widen)
            (Value::Int(n), Ty::Float) => {
                Ok(Value::Float(OrderedFloat(*n as f64)))
            }

            // Word -> Int (always safe)
            (Value::Word(n), Ty::Int) => Ok(Value::Int(*n as i64)),

            // Word -> Float (widen)
            (Value::Word(n), Ty::Float) => {
                Ok(Value::Float(OrderedFloat(*n as f64)))
            }

            // Float -> Int (truncate)
            (Value::Float(f), Ty::Int) => Ok(Value::Int(f.0 as i64)),

            // Bool -> Int
            (Value::Bool(b), Ty::Int) => Ok(Value::Int(if *b { 1 } else { 0 })),

            // T -> String (stringify)
            (_, Ty::String) => {
                let s = Self::stringify(ctx, val);
                let id = ctx.arena.intern(&s);
                Ok(Value::String(id))
            }

            // T -> Json (jsonify)
            (_, Ty::Json) => Ok(Value::Json(Arc::new(Self::jsonify(ctx, val)))),

            // String -> FilePath
            (Value::String(sid), Ty::FilePath) => Ok(Value::FilePath(*sid)),

            // DataStatus -> Int (variant idx to MUMPS value: 0, 1, 10, 11)
            (Value::Tagged(ty, idx, _), Ty::Int)
                if ctx.type_exprs.base_type(*ty)
                    == Some(TypeId::DATA_STATUS) =>
            {
                let mumps_val = match idx {
                    0 => 0,  // NoData
                    1 => 1,  // HasValue
                    2 => 10, // HasDescendants
                    3 => 11, // Both
                    _ => typechecked!("DataStatus AS Int", "valid variant"),
                };
                Ok(Value::Int(mumps_val))
            }

            // Path -> FilePath (extract filepath from either File or Dir variant)
            (Value::Tagged(ty, _, payloads), Ty::FilePath)
                if ctx.type_exprs.base_type(*ty) == Some(TypeId::PATH) =>
            {
                Ok(payloads
                    .first()
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("Path AS FilePath", "valid Path")
                    }))
            }

            // Range -> Array[Int]
            (
                Value::Range {
                    start,
                    end,
                    inclusive,
                },
                Ty::Named(id, _),
            ) if *id == TypeId::ARRAY => {
                let end = if *inclusive { *end + 1 } else { *end };
                let elem_ty = ctx.type_exprs.named(TypeId::INT);
                let elems = (*start..end)
                    .map(|n| ctx.arena.add(Value::Int(n), ctx.span))
                    .collect();
                Ok(Value::Array(elem_ty, Arc::new(elems)))
            }

            // Storable narrowing: `Storable AS T` where T is a Storable member.
            // This is the ONLY case that requires runtime type checking; all other
            // casts are validated by the type checker. If the value doesn't match
            // the target type, we return a runtime error.
            _ if matches!(
                target,
                Ty::Bool
                    | Ty::Int
                    | Ty::Float
                    | Ty::Char
                    | Ty::String
                    | Ty::Json
            ) && Self::is_storable_mismatch(val, target) =>
            {
                let src_name = Self::value_type_name(ctx, val);
                let tgt_name = Self::ty_name(target);
                Err(Error::runtime_type(
                    ctx.span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }

            // Compound types: identity cast only
            (_, _) => Ok(val.clone()),
        }
    }

    /// Check if a value is a Storable that doesn't match the target type.
    fn is_storable_mismatch(val: &Value, target: &Ty) -> bool {
        match (val, target) {
            (Value::Bool(_), Ty::Bool)
            | (Value::Int(_), Ty::Int)
            | (Value::Float(_), Ty::Float)
            | (Value::Char(_), Ty::Char)
            | (Value::String(_), Ty::String)
            | (Value::Json(_), Ty::Json) => false,
            // Value is a Storable type but doesn't match target
            (
                Value::Bool(_)
                | Value::Int(_)
                | Value::Float(_)
                | Value::Char(_)
                | Value::String(_)
                | Value::Json(_),
                _,
            ) => true,
            // Not a Storable type at all; don't trigger this branch
            _ => false,
        }
    }

    /// Get a human-readable name for a value's type.
    fn value_type_name(ctx: &ClassCtx<'_>, val: &Value) -> String {
        match val {
            Value::Unit => "Unit".to_owned(),
            Value::Bool(_) => "Bool".to_owned(),
            Value::Int(_) => "Int".to_owned(),
            Value::Word(_) => "Word".to_owned(),
            Value::Float(_) => "Float".to_owned(),
            Value::Char(_) => "Char".to_owned(),
            Value::String(_) => "String".to_owned(),
            Value::FilePath(_) => "FilePath".to_owned(),
            Value::Json(_) => "Json".to_owned(),
            Value::Array(_, _) => "Array".to_owned(),
            Value::Tuple(_, _) => "Tuple".to_owned(),
            Value::Object(_) => "Object".to_owned(),
            Value::Map(_, _, _) => "Map".to_owned(),
            Value::Time(_) => "Time".to_owned(),
            Value::Regex(_) => "Regex".to_owned(),
            Value::Range { .. } => "Range".to_owned(),
            Value::Tagged(ty_expr, _, _) => ctx
                .type_exprs
                .base_type(*ty_expr)
                .and_then(|ty| ctx.registry.type_name(ty, ctx.arena))
                .unwrap_or("Tagged")
                .to_owned(),
            Value::Closure { .. } => "Closure".to_owned(),
            Value::Function { .. } => "Function".to_owned(),
            Value::ModuleFn { .. } => "ModuleFn".to_owned(),
            Value::ClassMethodFn { .. } => "ClassMethodFn".to_owned(),
            Value::ModuleConst { .. } => "ModuleConst".to_owned(),
            Value::ForeverContinuation => "Continuation".to_owned(),
            Value::LoopContinue(_) => "LoopContinue".to_owned(),
            Value::Ref(is_global, _, _) => {
                if *is_global { "Global" } else { "Local" }.to_owned()
            }
            Value::Newtype(ty_expr, _) => {
                let ty = ctx
                    .type_exprs
                    .base_type(*ty_expr)
                    .unwrap_or_else(|| invariant!("newtype has base type"));
                ctx.registry
                    .type_name(ty, ctx.arena)
                    .unwrap_or_else(|| invariant!("newtype has type name"))
                    .to_owned()
            }
            Value::Union(ty_expr, _) => {
                // Unions can be either named (`union Result = Ok | Err`) or
                // anonymous (`String | Int`). Named unions have a type name
                // in the registry; anonymous unions need to be formatted as
                // their type expression (e.g., `"String | Int"`).
                ctx.type_exprs
                    .base_type(*ty_expr)
                    .and_then(|ty| {
                        ctx.registry
                            .type_name(ty, ctx.arena)
                            .map(|s| s.to_owned())
                    })
                    .unwrap_or_else(|| {
                        let arena = &*ctx.arena;
                        let registry = ctx.registry;
                        ctx.type_exprs
                            .format(
                                *ty_expr,
                                |ty| {
                                    registry
                                        .type_name(ty, arena)
                                        .unwrap_or_else(|| {
                                            invariant!("type in registry")
                                        })
                                        .to_owned()
                                },
                                |s| {
                                    arena
                                        .get_str(s)
                                        .unwrap_or_else(|| {
                                            invariant!("string in arena")
                                        })
                                        .to_owned()
                                },
                            )
                            .unwrap_or_else(|| {
                                invariant!("union type expr in arena")
                            })
                    })
            }
        }
    }

    /// Get a human-readable name for a type.
    fn ty_name(ty: &Ty) -> &'static str {
        match ty {
            Ty::Unit => "Unit",
            Ty::Bool => "Bool",
            Ty::Int => "Int",
            Ty::Word => "Word",
            Ty::Float => "Float",
            Ty::Char => "Char",
            Ty::String => "String",
            Ty::FilePath => "FilePath",
            Ty::Json => "Json",
            Ty::Named(_, _) => "Named",
            Ty::Tuple(_) => "Tuple",
            Ty::Object(_) => "Object",
            Ty::Fn(_, _) => "Function",
            Ty::Array(_) => "Array",
            Ty::Option(_) => "Option",
            Ty::Result(_, _) => "Result",
            Ty::Map(_, _) => "Map",
            Ty::Time => "Time",
            Ty::Range => "Range",
            Ty::Ordering => "Ordering",
            Ty::DataStatus => "DataStatus",
            Ty::Path => "Path",
            Ty::Regex => "Regex",
            Ty::RuntimeError => "RuntimeError",
            Ty::Local => "Local",
            Ty::Global => "Global",
            Ty::Union(_, _) => "Union",
            Ty::Var(_) => "Var",
            Ty::Apply(_, _) => "Apply",
            Ty::AssocType(_, _, _) => "AssocType",
            Ty::Unknown => "Unknown",
            Ty::Error => "Error",
        }
    }

    /// Stringify a value to produce raw string content (not quoted).
    fn coerce_to_str(ctx: &ClassCtx<'_>, v: &Value) -> String {
        match v {
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => ctx
                .arena
                .get(*inner_id)
                .map(|inner| Self::coerce_to_str(ctx, inner))
                .unwrap_or_else(|| Display::format(ctx, v)),
            Value::String(id) | Value::FilePath(id) => {
                ctx.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => Display::format(ctx, v),
        }
    }

    /// Stringify a value for `AS String` conversion.
    fn stringify(ctx: &ClassCtx<'_>, v: &Value) -> String {
        Self::coerce_to_str(ctx, v)
    }

    /// Convert a value to JSON.
    ///
    /// Returns the JSON directly; for the class method wrapper that returns
    /// `Value::Json`, dispatch to `Into[Json]` via `Into::into`.
    pub(crate) fn jsonify(ctx: &ClassCtx<'_>, v: &Value) -> serde_json::Value {
        match v {
            Value::Unit => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(n) => serde_json::json!(*n),
            Value::Word(n) => serde_json::json!(*n),
            Value::Float(f) => serde_json::json!(f.0),
            Value::Char(c) => serde_json::Value::String(c.to_string()),
            Value::String(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Value::FilePath(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Value::Array(_, arr) => {
                let elems: Vec<_> = arr
                    .iter()
                    .map(|id| {
                        ctx.arena
                            .get(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| Self::jsonify(ctx, v))
                    .collect();
                serde_json::Value::Array(elems)
            }
            Value::Tuple(_, elems) => {
                let items: Vec<_> = elems
                    .iter()
                    .map(|id| {
                        ctx.arena
                            .get(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| Self::jsonify(ctx, v))
                    .collect();
                serde_json::Value::Array(items)
            }
            Value::Object(obj) => {
                let map: serde_json::Map<_, _> = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = ctx
                            .arena
                            .get_str(*k)
                            .unwrap_or_else(|| invariant!("StringId in arena"));
                        let val = ctx
                            .arena
                            .get(*vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key.to_owned(), Self::jsonify(ctx, val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = ctx.type_exprs.base_type(*ty_expr);

                // Option encodes as null/value rather than tagged object
                if base_ty.is_some_and(|ty| ty == TypeId::OPTION) {
                    if *idx == 0 {
                        serde_json::Value::Null
                    } else {
                        payloads
                            .first()
                            .and_then(|id| ctx.arena.get(*id))
                            .map(|v| Self::jsonify(ctx, v))
                            .unwrap_or(serde_json::Value::Null)
                    }
                } else {
                    let ty_name = base_ty
                        .and_then(|ty| ctx.registry.type_name(ty, ctx.arena))
                        .unwrap_or("?");
                    let var_name = base_ty
                        .and_then(|ty| {
                            ctx.registry.variant_name(ty, *idx, ctx.arena)
                        })
                        .unwrap_or("?");

                    let payload_json = if payloads.is_empty() {
                        serde_json::Value::Null
                    } else if payloads.len() == 1 {
                        payloads
                            .first()
                            .and_then(|id| ctx.arena.get(*id))
                            .map(|v| Self::jsonify(ctx, v))
                            .unwrap_or(serde_json::Value::Null)
                    } else {
                        let items: Vec<_> = payloads
                            .iter()
                            .map(|id| {
                                ctx.arena.get(*id).unwrap_or_else(|| {
                                    invariant!("ValueId in arena")
                                })
                            })
                            .map(|v| Self::jsonify(ctx, v))
                            .collect();
                        serde_json::Value::Array(items)
                    };

                    serde_json::json!({
                        "type": ty_name,
                        "variant": var_name,
                        "payload": payload_json
                    })
                }
            }
            Value::Map(_, _, entries) => {
                let map: serde_json::Map<_, _> = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = Self::jsonify_map_key(ctx, k);
                        let val = ctx
                            .arena
                            .get(*vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key, Self::jsonify(ctx, val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            Value::Time(t) => serde_json::Value::String(t.to_rfc3339()),
            Value::Json(j) => j.as_ref().clone(),
            Value::Regex(idx) => {
                let pattern = ctx
                    .regex_cache
                    .get(*idx as usize)
                    .map(|r| r.as_str())
                    .unwrap_or("?");
                serde_json::Value::String(pattern.to_owned())
            }
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                serde_json::json!({
                    "start": *start,
                    "end": *end,
                    "inclusive": *inclusive
                })
            }
            Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::ClassMethodFn { .. }
            | Value::ModuleConst { .. }
            | Value::ForeverContinuation
            | Value::LoopContinue(_) => serde_json::Value::Null,
            Value::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = ctx.arena.get_str(*name_id).unwrap_or("?");
                let subs: Vec<_> = sub_ids
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::jsonify(ctx, v))
                    .collect();
                serde_json::json!({
                    "ref": format!("{prefix}{name}"),
                    "subscripts": subs
                })
            }
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => {
                let inner = ctx
                    .arena
                    .get(*inner_id)
                    .unwrap_or_else(|| invariant!("inner value in arena"));
                Self::jsonify(ctx, inner)
            }
        }
    }

    /// Convert a map key to a JSON-compatible string key.
    fn jsonify_map_key(ctx: &ClassCtx<'_>, k: &crate::value::MapKey) -> String {
        use crate::value::MapKey;
        match k {
            MapKey::Bool(b) => b.to_string(),
            MapKey::Int(n) => n.to_string(),
            MapKey::Float(f) => f.to_string(),
            MapKey::Char(c) => c.to_string(),
            MapKey::String(id) => {
                ctx.arena.get_str(*id).unwrap_or("").to_owned()
            }
        }
    }
}

/// Fallible conversion: `T: TryInto[U]` means `T` might convert to `U`.
pub(crate) struct TryInto;

impl Class for TryInto {}

impl TryInto {
    /// Try to convert a value to the target type.
    ///
    /// Returns `Result[U, String]` where `Err` contains an error message.
    /// Mirrors the `read` operator behavior exactly.
    pub(crate) fn try_into(
        ctx: &mut ClassCtx<'_>,
        val: &Value,
        target: &Ty,
    ) -> Result<Value> {
        match (val, target) {
            // Same type: identity conversion always succeeds
            _ if Self::types_match(val, target) => {
                Ok(Self::make_result_ok(ctx, val.clone()))
            }

            // String -> Int
            (Value::String(sid), Ty::Int) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<i64>() {
                    Ok(n) => Self::make_result_ok(ctx, Value::Int(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid integer: {s}"),
                    ),
                })
            }

            // String -> Float
            (Value::String(sid), Ty::Float) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<f64>() {
                    Ok(n) => {
                        Self::make_result_ok(ctx, Value::Float(OrderedFloat(n)))
                    }
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid float: {s}"),
                    ),
                })
            }

            // String -> Word
            (Value::String(sid), Ty::Word) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<usize>() {
                    Ok(n) => Self::make_result_ok(ctx, Value::Word(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid unsigned integer: {s}"),
                    ),
                })
            }

            // Int -> Bool (strict: only 0 and 1)
            (Value::Int(n), Ty::Bool) => Ok(match *n {
                0 => Self::make_result_ok(ctx, Value::Bool(false)),
                1 => Self::make_result_ok(ctx, Value::Bool(true)),
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected 0 or 1 for Bool, got {n}"),
                ),
            }),

            // Int -> Word (must be non-negative)
            (Value::Int(n), Ty::Word) => Ok(if *n >= 0 {
                Self::make_result_ok(ctx, Value::Word(*n as usize))
            } else {
                Self::make_result_err(
                    ctx,
                    &format!("expected non-negative Int for Word, got {n}"),
                )
            }),

            // Json -> Bool
            (Value::Json(j), Ty::Bool) => Ok(match &**j {
                serde_json::Value::Bool(b) => {
                    Self::make_result_ok(ctx, Value::Bool(*b))
                }
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Bool, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Bool, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> Int
            (Value::Json(j), Ty::Int) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_i64()
                    .map(|i| Self::make_result_ok(ctx, Value::Int(i)))
                    .unwrap_or_else(|| {
                        Self::make_result_err(
                            ctx,
                            &format!(
                                "expected Int, got non-integer number {n}"
                            ),
                        )
                    }),
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Int, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Int, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> Float
            (Value::Json(j), Ty::Float) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_f64()
                    .map(|f| {
                        Self::make_result_ok(ctx, Value::Float(OrderedFloat(f)))
                    })
                    .unwrap_or_else(|| {
                        Self::make_result_err(
                            ctx,
                            &format!("expected Float, got invalid number {n}"),
                        )
                    }),
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Float, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Float, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> String
            (Value::Json(j), Ty::String) => Ok(match &**j {
                serde_json::Value::String(s) => {
                    let id = ctx.arena.intern(s);
                    Self::make_result_ok(ctx, Value::String(id))
                }
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected String, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!(
                        "expected String, got {}",
                        Self::json_type_name(j)
                    ),
                ),
            }),

            // T -> Json (jsonify)
            (_, Ty::Json) => Ok(Self::make_result_ok(
                ctx,
                Value::Json(Arc::new(Into::jsonify(ctx, val))),
            )),

            // Int -> DataStatus (MUMPS @data values: 0, 1, 10, 11 -> variants)
            (Value::Int(n), Ty::DataStatus) => {
                let (variant_idx, valid) = match *n {
                    0 => (0, true),  // NoData
                    1 => (1, true),  // HasValue
                    10 => (2, true), // HasDescendants
                    11 => (3, true), // Both
                    _ => (0, false),
                };

                Ok(if valid {
                    let ty_expr = ctx.type_exprs.named(TypeId::DATA_STATUS);
                    Self::make_result_ok(
                        ctx,
                        Value::Tagged(ty_expr, variant_idx, SmallVec::new()),
                    )
                } else {
                    Self::make_result_err(
                        ctx,
                        &format!(
                            "invalid DataStatus value: {n} (expected 0, 1, 10, or 11)"
                        ),
                    )
                })
            }

            // Unsupported conversion: return Result.Err
            _ => {
                let src = Into::value_type_name(ctx, val);
                let tgt = Into::ty_name(target);
                let msg = format!("cannot read {src} as {tgt}");
                Ok(Self::make_result_err(ctx, &msg))
            }
        }
    }

    /// Check if a value's runtime type matches the target type.
    fn types_match(val: &Value, target: &Ty) -> bool {
        matches!(
            (val, target),
            (Value::Bool(_), Ty::Bool)
                | (Value::Int(_), Ty::Int)
                | (Value::Word(_), Ty::Word)
                | (Value::Float(_), Ty::Float)
                | (Value::Char(_), Ty::Char)
                | (Value::String(_), Ty::String)
                | (Value::FilePath(_), Ty::FilePath)
                | (Value::Json(_), Ty::Json)
                | (Value::Unit, Ty::Unit)
        )
    }

    /// Get human-readable JSON type name.
    fn json_type_name(j: &serde_json::Value) -> &'static str {
        match j {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    /// Create a `Result.Ok(val)` value.
    fn make_result_ok(ctx: &mut ClassCtx<'_>, val: Value) -> Value {
        let val_id = ctx.arena.add(val, ctx.span);
        let ok_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
        let err_ty = ctx.type_exprs.named(TypeId::STRING);
        let result_ty =
            ctx.type_exprs.app(TypeId::RESULT, smallvec![ok_ty, err_ty]);
        Value::Tagged(result_ty, 0, smallvec![val_id])
    }

    /// Create a `Result.Err(msg)` value.
    fn make_result_err(ctx: &mut ClassCtx<'_>, msg: &str) -> Value {
        let msg_id = ctx.arena.intern(msg);
        let msg_val_id = ctx.arena.add(Value::String(msg_id), ctx.span);
        let ok_ty = ctx.type_exprs.named(TypeId::UNKNOWN);
        let err_ty = ctx.type_exprs.named(TypeId::STRING);
        let result_ty =
            ctx.type_exprs.app(TypeId::RESULT, smallvec![ok_ty, err_ty]);
        Value::Tagged(result_ty, 1, smallvec![msg_val_id])
    }
}

/// Display formatting: produces valid RUMPS syntax (for `write`).
pub(crate) struct Display;

impl Class for Display {}

impl Display {
    /// Format a value as valid RUMPS syntax (strings quoted).
    ///
    /// Unlike `Into[String]` which produces raw string content, this produces
    /// output suitable for display (e.g., `write` statements) where strings
    /// are quoted and complex types are formatted for readability.
    pub(crate) fn display(ctx: &mut ClassCtx<'_>, v: &Value) -> Result<Value> {
        let s = Self::format(ctx, v);
        let id = ctx.arena.intern(&s);
        Ok(Value::String(id))
    }

    /// Format a value as valid RUMPS syntax.
    ///
    /// Returns the string directly; for the class method wrapper that returns
    /// `Value::String`, see [`display`](Self::display).
    pub(crate) fn format(ctx: &ClassCtx<'_>, v: &Value) -> String {
        match v {
            Value::Unit => "Unit".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Word(n) => n.to_string(),
            Value::Float(f) => {
                let s = f.to_string();
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
            Value::Char(c) => format!("'{c}'"),
            Value::String(id) | Value::FilePath(id) => {
                let s = ctx.arena.get_str(*id).unwrap_or("");
                format!("\"{s}\"")
            }
            Value::Regex(idx) => {
                let re =
                    ctx.regex_cache.get(*idx as usize).unwrap_or_else(|| {
                        typechecked!("Display", "valid Regex cache index")
                    });
                format!("/{}/", re.as_str())
            }
            Value::Array(_, elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[ {items} ]")
            }
            Value::Tuple(_, elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .collect::<Vec<_>>()
                    .join(", ");
                let trail = if elems.len() == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Value::Object(obj) => {
                let fields = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = ctx.arena.get_str(*k).unwrap_or("?");
                        let val = ctx
                            .arena
                            .get(*vid)
                            .map(|v| Self::format(ctx, v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key}: {val}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Value::Map(_, _, entries) => {
                let items = entries
                    .iter()
                    .map(|(k, vid)| {
                        let key = Self::format_map_key(ctx, k);
                        let val = ctx
                            .arena
                            .get(*vid)
                            .map(|v| Self::format(ctx, v))
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{key} => {val}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {items} }}")
            }
            Value::Time(t) => t.to_rfc3339(),
            Value::Json(j) => j.to_string(),
            Value::Tagged(ty_expr, idx, payloads) => {
                let base_ty = ctx.type_exprs.base_type(*ty_expr);
                let ty_name = base_ty
                    .and_then(|ty| ctx.registry.type_name(ty, ctx.arena))
                    .unwrap_or("?");
                let var_name = base_ty
                    .and_then(|ty| {
                        ctx.registry.variant_name(ty, *idx, ctx.arena)
                    })
                    .unwrap_or("?");

                if payloads.is_empty() {
                    format!("{ty_name}.{var_name}")
                } else {
                    let args = payloads
                        .iter()
                        .filter_map(|id| ctx.arena.get(*id))
                        .map(|v| Self::format(ctx, v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{ty_name}.{var_name}({args})")
                }
            }
            Value::Closure { .. } => {
                typechecked!("Display", "Display (not Closure)")
            }
            Value::Function { .. } => {
                typechecked!("Display", "Display (not Function)")
            }
            Value::ModuleFn { .. } => {
                typechecked!("Display", "Display (not ModuleFn)")
            }
            Value::ClassMethodFn { .. } => {
                typechecked!("Display", "Display (not ClassMethodFn)")
            }
            Value::ModuleConst { path } => {
                let path_str: String = path
                    .iter()
                    .filter_map(|id| ctx.arena.get_str(*id))
                    .collect::<Vec<_>>()
                    .join(".");
                format!("<{path_str}>")
            }
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                if *inclusive {
                    format!("{start} ..= {end}")
                } else {
                    format!("{start} .. {end}")
                }
            }
            Value::ForeverContinuation => "<continuation>".into(),
            Value::LoopContinue(_) => "<loop-continue>".into(),
            Value::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = ctx.arena.get_str(*name_id).unwrap_or("?");
                let subs = sub_ids
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{prefix}{name}{{{subs}}}")
            }
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => {
                let inner = ctx
                    .arena
                    .get(*inner_id)
                    .unwrap_or_else(|| invariant!("inner value in arena"));
                Self::format(ctx, inner)
            }
        }
    }

    /// Format a map key for display.
    fn format_map_key(ctx: &ClassCtx<'_>, k: &crate::value::MapKey) -> String {
        use crate::value::MapKey;
        match k {
            MapKey::Bool(b) => b.to_string(),
            MapKey::Int(n) => n.to_string(),
            MapKey::Float(f) => f.to_string(),
            MapKey::Char(c) => format!("'{c}'"),
            MapKey::String(id) => ctx
                .arena
                .get_str(*id)
                .map(|s| format!("\"{s}\""))
                .unwrap_or_else(|| "\"?\"".to_owned()),
        }
    }
}

/// `Mappable` class: `map` method.
pub(crate) struct Mappable;

impl Mappable {
    /// Start `Mappable:map`; returns first invocation or done for empty.
    ///
    /// Handles iterables (Array, Range) and single-value containers (Option, Result).
    pub(crate) fn map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Mappable:map", "2 args"));
        let src = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Mappable:map", "2 args"));

        // Extract data to avoid borrow conflicts
        enum Kind {
            EmptyArray,
            Array(ValueId),
            OptionSome(ValueId),
            OptionNone(TypeExprId),
            ResultOk(ValueId, TypeExprId), // inner, err_ty
            ResultErr(Value),
            Other,
        }
        let kind = match ctx.arena.get(src) {
            Some(Value::Array(_, elems)) if elems.is_empty() => {
                Kind::EmptyArray
            }
            Some(Value::Array(_, elems)) => Kind::Array(elems[0]),
            // Option.Some(v) -> map inner
            Some(Value::Tagged(ty, 1, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                Kind::OptionSome(
                    *payloads
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            // Option.None -> return None
            Some(Value::Tagged(ty, 0, _))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                Kind::OptionNone(*ty)
            }
            // Result.Ok(v) -> map inner
            Some(Value::Tagged(ty, 0, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                let inner = *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Ok has payload"));
                // Extract error type from Result[T, E]
                let err_ty = ctx
                    .type_exprs
                    .type_args(*ty)
                    .and_then(|args| args.get(1).copied())
                    .unwrap_or_else(|| ctx.type_exprs.named(TypeId::UNKNOWN));
                Kind::ResultOk(inner, err_ty)
            }
            // Result.Err(e) -> return unchanged
            Some(v @ Value::Tagged(ty, 1, _))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                Kind::ResultErr(v.clone())
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::EmptyArray => {
                let ty = ctx.type_exprs.named(TypeId::UNKNOWN);
                Ok(MethodResult::Done(Value::Array(
                    ty,
                    Arc::new(SmallVec::new()),
                )))
            }
            Kind::Array(first) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![first],
                state: HofState::MapIter {
                    kind: IterKind::Array {
                        source: src,
                        idx: 0,
                    },
                    acc: SmallVec::new(),
                    elem_ty: None,
                },
            })),
            Kind::OptionSome(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::MapContainer {
                    ctor_ty: TypeId::OPTION,
                    tag: 1, // Some
                    extra_ty: None,
                },
            })),
            Kind::OptionNone(ty) => Ok(MethodResult::Done(Value::none(ty))),
            Kind::ResultOk(inner, err_ty) => {
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![inner],
                    state: HofState::MapContainer {
                        ctor_ty: TypeId::RESULT,
                        tag: 0, // Ok
                        extra_ty: Some(err_ty),
                    },
                }))
            }
            Kind::ResultErr(v) => Ok(MethodResult::Done(v)),
            Kind::Other => typechecked!("Mappable:map", "Mappable"),
        }
    }
}

/// `Filterable` class: `filter` method.
pub(crate) struct Filterable;

impl Filterable {
    /// Start `Filterable:filter`; returns first invocation or done for empty.
    pub(crate) fn filter(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let pred_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Filterable:filter", "2 args"));
        let src = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Filterable:filter", "2 args"));

        enum Kind {
            EmptyArray(TypeExprId),
            Array(TypeExprId, ValueId),
            EmptyRange,
            Range(i64, i64),
            Other,
        }
        let kind = match ctx.arena.get(src) {
            Some(Value::Array(ty, elems)) if elems.is_empty() => {
                Kind::EmptyArray(*ty)
            }
            Some(Value::Array(ty, elems)) => Kind::Array(*ty, elems[0]),
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => {
                let e = if *inclusive { *end + 1 } else { *end };
                if *start >= e {
                    Kind::EmptyRange
                } else {
                    Kind::Range(*start, e)
                }
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::EmptyArray(ty) => Ok(MethodResult::Done(Value::Array(
                ty,
                Arc::new(SmallVec::new()),
            ))),
            Kind::Array(elem_ty, first) => {
                Ok(MethodResult::Invoke(Continuation {
                    callee: pred_id,
                    args: smallvec![first],
                    state: HofState::FilterArray {
                        source: src,
                        idx: 0,
                        elem_ty,
                        acc: SmallVec::new(),
                        pending: first,
                    },
                }))
            }
            Kind::EmptyRange => {
                let int_ty = ctx.type_exprs.named(TypeId::INT);
                Ok(MethodResult::Done(Value::Array(
                    int_ty,
                    Arc::new(SmallVec::new()),
                )))
            }
            Kind::Range(start, end) => {
                let int_ty = ctx.type_exprs.named(TypeId::INT);
                let int_id = ctx.arena.add(Value::Int(start), ctx.span);
                Ok(MethodResult::Invoke(Continuation {
                    callee: pred_id,
                    args: smallvec![int_id],
                    state: HofState::FilterRange {
                        current: start + 1,
                        end,
                        elem_ty: int_ty,
                        acc: SmallVec::new(),
                        pending: start,
                    },
                }))
            }
            Kind::Other => typechecked!("Filterable:filter", "Array or Range"),
        }
    }
}

/// `Foldable` class: `reduce` method.
pub(crate) struct Foldable;

impl Foldable {
    /// Start `Foldable:reduce`; returns first invocation.
    pub(crate) fn reduce(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Foldable:reduce", "3 args"));
        let init = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Foldable:reduce", "3 args"));
        let src = *args
            .get(2)
            .unwrap_or_else(|| typechecked!("Foldable:reduce", "3 args"));

        // Extract data before second match to satisfy borrow checker.
        enum Kind {
            EmptyArray,
            Array(ValueId),
            EmptyRange,
            Range(i64, i64),
            Other,
        }
        // Helper to unwrap Union/Newtype to get inner value
        fn unwrap_src(arena: &ValueArena, id: ValueId) -> Option<&Value> {
            arena.get(id).and_then(|v| match v {
                Value::Union(_, inner) | Value::Newtype(_, inner) => {
                    unwrap_src(arena, *inner)
                }
                other => Some(other),
            })
        }
        let kind = match unwrap_src(ctx.arena, src) {
            Some(Value::Array(_, elems)) if elems.is_empty() => {
                Kind::EmptyArray
            }
            Some(Value::Array(_, elems)) => Kind::Array(elems[0]),
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => {
                let actual = if *inclusive { *end + 1 } else { *end };
                if *start >= actual {
                    Kind::EmptyRange
                } else {
                    Kind::Range(*start, actual)
                }
            }
            _ => Kind::Other,
        };
        match kind {
            Kind::EmptyArray | Kind::EmptyRange => {
                let v = ctx
                    .arena
                    .get(init)
                    .cloned()
                    .unwrap_or_else(|| invariant!("init in arena"));
                Ok(MethodResult::Done(v))
            }
            Kind::Array(first) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![init, first],
                state: HofState::ReduceArray {
                    source: src,
                    idx: 0,
                    acc: init,
                },
            })),
            Kind::Range(start, end) => {
                let int_id = ctx.arena.add(Value::Int(start), ctx.span);
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![init, int_id],
                    state: HofState::ReduceRange {
                        current: start + 1,
                        end,
                        acc: init,
                    },
                }))
            }
            Kind::Other => typechecked!("Foldable:reduce", "Array or Range"),
        }
    }
}

/// `Iterable` class: `length`, `collect` methods.
pub(crate) struct Iterable;

impl Iterable {
    /// `Iterable:length`; returns the number of elements.
    pub(crate) fn length(_: &mut ClassCtx<'_>, v: &Value) -> Result<Value> {
        Ok(match v {
            Value::Array(_, elems) => Value::Int(elems.len() as i64),
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                let len = if *inclusive {
                    end - start + 1
                } else {
                    end - start
                };
                Value::Int(len.max(0))
            }
            _ => typechecked!("Iterable:length", "Iterable"),
        })
    }

    /// `Iterable:collect`; materializes an iterable into an `Array`.
    pub(crate) fn collect(ctx: &mut ClassCtx<'_>, v: &Value) -> Result<Value> {
        Ok(match v {
            Value::Array(ty, elems) => Value::Array(*ty, elems.clone()),
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                let actual_end = if *inclusive { *end + 1 } else { *end };
                let elems: SmallVec<[ValueId; 4]> = (*start..actual_end)
                    .map(|i| ctx.arena.add(Value::Int(i), ctx.span))
                    .collect();
                let ty = ctx.type_exprs.named(TypeId::INT);
                Value::Array(ty, Arc::new(elems))
            }
            _ => typechecked!("Iterable:collect", "Iterable"),
        })
    }
}

/// `Chainable` class: `chain` method.
impl Chainable {
    /// Start `Chainable:chain`; single invocation for Some/Ok, or done for None/Err.
    pub(crate) fn chain(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let src = *args
            .first()
            .unwrap_or_else(|| typechecked!("Chainable:chain", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Chainable:chain", "2 args"));

        match ctx.arena.get(src) {
            // Option.None -> None
            Some(Value::Tagged(ty, 0, _))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                Ok(MethodResult::Done(Value::none(*ty)))
            }
            // Option.Some(v) -> invoke fn(v)
            Some(Value::Tagged(ty, 1, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::OPTION) =>
            {
                let inner = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Some has payload"));
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![inner],
                    state: HofState::Chain {
                        wrapper: ChainWrapper::OptionSome,
                    },
                }))
            }
            // Result.Ok(v) -> invoke fn(v)
            Some(Value::Tagged(ty, 0, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                let inner = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Ok has payload"));
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![inner],
                    state: HofState::Chain {
                        wrapper: ChainWrapper::ResultOk,
                    },
                }))
            }
            // Result.Err(e) -> propagate error unchanged
            Some(Value::Tagged(ty, 1, payloads))
                if ctx
                    .type_exprs
                    .base_type(*ty)
                    .is_some_and(|t| t == TypeId::RESULT) =>
            {
                let err = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                Ok(MethodResult::Done(Value::Tagged(*ty, 1, smallvec![err])))
            }
            _ => typechecked!("Chainable:chain", "Option or Result"),
        }
    }
}
