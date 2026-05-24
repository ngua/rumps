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
//! - `Bimappable`: `bimap`
//!
//! Higher-order class methods use a continuation/trampoline pattern defined in
//! the [`hof`](super::hof) module.
//!
//! [`Interpreter`]: super::Interpreter

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use itertools::Itertools;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use super::hof::{
    ChainWrapper, Continuation, HofMethodFn, HofState, IterKind, MethodResult,
};
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{RuntimeTypes, Ty, TyArena};
use crate::value::{
    MapKey, Payload, TypeId, TypeRegistry, ValueArena, ValueId, ValueMeta,
};
use crate::{ClassId, Error, Result, Span};

/// Context for class method dispatch.
pub(crate) struct ClassCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) ty_arena: &'a TyArena,
    pub(crate) runtime_types: &'a RuntimeTypes,
    pub(crate) registry: &'a TypeRegistry,
    pub(crate) regex_cache: &'a [regex::Regex],
    pub(crate) span: Span,
}

/// Binary class method signature.
pub(crate) type BinMethodFn =
    fn(&mut ClassCtx<'_>, &Payload, &Payload) -> Result<Payload>;

/// Unary class method signature.
pub(crate) type UnaryMethodFn =
    fn(&mut ClassCtx<'_>, &Payload) -> Result<Payload>;

/// Nullary class method signature (e.g., `Monoid::identity`).
///
/// Takes the statically-inferred type to produce the appropriate value.
pub(crate) type NullaryMethodFn = fn(&mut ClassCtx<'_>, &Ty) -> Result<Payload>;

/// Conversion method signature (e.g., `Into::into`, `TryInto::try_into`).
///
/// Takes a value and the target type to convert to.
pub(crate) type ConvertMethodFn =
    fn(&mut ClassCtx<'_>, &Payload, &Ty) -> Result<Payload>;

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
        recv: &Payload,
        arg: &Payload,
    ) -> Result<Payload> {
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
        recv: &Payload,
    ) -> Result<Payload> {
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
    ) -> Result<Payload> {
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
        val: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
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
        self.register(
            ClassId::BIMAPPABLE,
            i.intern("bimap"),
            MethodFn::Hof(Bimappable::bimap),
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
    fn map_key(v: &Payload) -> MapKey {
        MapKey::from_payload(v)
            .unwrap_or_else(|| typechecked!("map key", "valid key type"))
    }
}

/// Arithmetic operations for `Int`, `Word`, `Float`.
pub(crate) struct Numeric;

impl Class for Numeric {}

impl Numeric {
    pub(crate) fn add(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_add(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_add(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 + b.0))
            }
            _ => typechecked!("+", "same Numeric type"),
        })
    }

    pub(crate) fn sub(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_sub(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_sub(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 - b.0))
            }
            _ => typechecked!("-", "same Numeric type"),
        })
    }

    pub(crate) fn mul(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_mul(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_mul(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 * b.0))
            }
            _ => typechecked!("*", "same Numeric type"),
        })
    }

    pub(crate) fn floor_div(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Int(a.div_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Word(a / b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat((a.0 / b.0).floor())))
                }
            }
            _ => typechecked!("//", "same Numeric type"),
        }
    }

    pub(crate) fn modulo(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Int(a.rem_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Word(a % b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat(a.0 % b.0)))
                }
            }
            _ => typechecked!("%", "same Numeric type"),
        }
    }

    pub(crate) fn pow(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(base), Payload::Int(exp)) => {
                if *exp < 0 {
                    Payload::Float(OrderedFloat(
                        (*base as f64).powf(*exp as f64),
                    ))
                } else {
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| base.checked_pow(e))
                        .map_or_else(
                            || {
                                Payload::Float(OrderedFloat(
                                    (*base as f64).powf(*exp as f64),
                                ))
                            },
                            Payload::Int,
                        )
                }
            }
            (Payload::Word(base), Payload::Word(exp)) => Payload::Word(
                u32::try_from(*exp)
                    .ok()
                    .and_then(|e| base.checked_pow(e))
                    .unwrap_or(usize::MAX),
            ),
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0.powf(b.0)))
            }
            _ => typechecked!("**", "same Numeric type"),
        })
    }
}

/// Unary negation for `Int`, `Float`.
pub(crate) struct Negatable;

impl Class for Negatable {}

impl Negatable {
    pub(crate) fn neg(_: &mut ClassCtx<'_>, v: &Payload) -> Result<Payload> {
        Ok(match v {
            Payload::Int(n) => Payload::Int(-n),
            Payload::Float(f) => Payload::Float(OrderedFloat(-f.0)),
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
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a && *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a & b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a & b),
            _ => typechecked!("&", "BitLike"),
        })
    }

    pub(crate) fn or(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a || *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a | b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a | b),
            _ => typechecked!("|", "BitLike"),
        })
    }

    pub(crate) fn shl(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shl((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shl((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!("<<", "BitLike"),
        })
    }

    pub(crate) fn shr(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shr((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shr((*b as u32) & (usize::BITS - 1)))
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
    pub(crate) fn identity(ctx: &mut ClassCtx<'_>, ty: &Ty) -> Result<Payload> {
        Ok(match ty {
            Ty::String => Payload::String(ctx.arena.intern("")),
            Ty::Array(_) => Payload::Array(Arc::new(SmallVec::new())),
            Ty::Map(_, _) => Payload::Map(Arc::new(IndexMap::new())),
            Ty::Option(_) => Payload::none(),
            _ => typechecked!("identity", "Monoid type"),
        })
    }

    pub(crate) fn concat(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::String(ls), Payload::String(rs)) => {
                let l = ctx.arena.get_str(*ls).unwrap_or("");
                let r = ctx.arena.get_str(*rs).unwrap_or("");
                Payload::String(ctx.arena.intern(&format!("{l}{r}")))
            }
            (Payload::Array(l), Payload::Array(r)) => {
                let mut elems = Arc::unwrap_or_clone(l.clone());
                elems.extend(r.iter().copied());
                Payload::Array(Arc::new(elems))
            }
            (Payload::Map(l), Payload::Map(r)) => {
                let mut merged = Arc::unwrap_or_clone(l.clone());
                merged.extend(r.iter().map(|(k, v)| (k.clone(), *v)));
                Payload::Map(Arc::new(merged))
            }
            (Payload::Tagged(ty1, i1, p1), Payload::Tagged(ty2, i2, p2))
                if *ty1 == TypeId::OPTION && *ty2 == TypeId::OPTION =>
            {
                if *i1 == 1 {
                    Payload::Tagged(*ty1, *i1, p1.clone())
                } else if *i2 == 1 {
                    Payload::Tagged(*ty2, *i2, p2.clone())
                } else {
                    Payload::Tagged(*ty1, *i1, SmallVec::new())
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
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        let ord = Self::cmp_values(ctx, l, r);
        Ok(Payload::Int(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    }

    /// Recursive comparison helper returning `Ordering`.
    fn cmp_values(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Ordering {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => a.cmp(b),
            (Payload::Word(a), Payload::Word(b)) => a.cmp(b),
            (Payload::Float(a), Payload::Float(b)) => a.cmp(b),
            (Payload::String(a), Payload::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa.cmp(sb)
            }
            (Payload::Char(a), Payload::Char(b)) => a.cmp(b),
            (Payload::Bool(a), Payload::Bool(b)) => a.cmp(b),
            (Payload::Time(a), Payload::Time(b)) => a.cmp(b),
            // Arrays: lexicographic comparison
            (Payload::Array(a), Payload::Array(b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            // Tuples: lexicographic comparison
            (Payload::Tuple(a), Payload::Tuple(b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            // Maps: lexicographic comparison by (key, value) pairs sorted by key
            (Payload::Map(a), Payload::Map(b)) => Self::cmp_maps(ctx, a, b),
            // Tagged (Option, Result, user types): compare variant index, then payload
            // Note: Result has Ok=0, Err=1, but we want Err < Ok, so reverse for Result
            (Payload::Tagged(ty, i1, p1), Payload::Tagged(_, i2, p2)) => {
                let is_result = *ty == TypeId::RESULT;
                let idx_ord = if is_result { i2.cmp(i1) } else { i1.cmp(i2) };
                match idx_ord {
                    Ordering::Equal => {
                        Self::cmp_seqs(ctx, p1.as_slice(), p2.as_slice())
                    }
                    ord => ord,
                }
            }
            _ => typechecked!("compare", "same Ord type"),
        }
    }

    /// Lexicographic comparison of sequences of `ValueId`s.
    fn cmp_seqs(
        ctx: &mut ClassCtx<'_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(ai, bi)| {
                let av = ctx.arena.get(*ai).cloned();
                let bv = ctx.arena.get(*bi).cloned();
                match (av, bv) {
                    (Some(av), Some(bv)) => Self::cmp_values(ctx, &av, &bv),
                    _ => Ordering::Equal,
                }
            })
            .find(|o| *o != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    }

    /// Compare two maps by sorting entries by key, then comparing lexicographically.
    fn cmp_maps(
        ctx: &mut ClassCtx<'_>,
        a: &IndexMap<MapKey, ValueId>,
        b: &IndexMap<MapKey, ValueId>,
    ) -> Ordering {
        // Collect and sort entries by key (convert MapKey to Payload for comparison)
        let mut a_entries: Vec<_> =
            a.iter().map(|(k, v)| (k.to_payload(), *v)).collect();
        let mut b_entries: Vec<_> =
            b.iter().map(|(k, v)| (k.to_payload(), *v)).collect();
        a_entries.sort_by(|(k1, _), (k2, _)| Self::cmp_values(ctx, k1, k2));
        b_entries.sort_by(|(k1, _), (k2, _)| Self::cmp_values(ctx, k1, k2));
        // Compare lexicographically by (key, value) pairs
        a_entries
            .iter()
            .zip(b_entries.iter())
            .map(|((k1, v1), (k2, v2))| {
                let key_ord = Self::cmp_values(ctx, k1, k2);
                if key_ord != Ordering::Equal {
                    key_ord
                } else {
                    let v1 = ctx.arena.get(*v1).cloned();
                    let v2 = ctx.arena.get(*v2).cloned();
                    match (v1, v2) {
                        (Some(v1), Some(v2)) => Self::cmp_values(ctx, &v1, &v2),
                        _ => Ordering::Equal,
                    }
                }
            })
            .find(|o| *o != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    }
}

/// Equality comparison.
pub(crate) struct Eq;

impl Class for Eq {}

impl Eq {
    pub(crate) fn eq(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(Payload::Bool(Self::values_equal(ctx, l, r)))
    }

    /// Recursive equality helper.
    fn values_equal(ctx: &mut ClassCtx<'_>, l: &Payload, r: &Payload) -> bool {
        match (l, r) {
            (Payload::Unit, Payload::Unit) => true,
            (Payload::Bool(a), Payload::Bool(b)) => a == b,
            (Payload::Int(a), Payload::Int(b)) => a == b,
            (Payload::Word(a), Payload::Word(b)) => a == b,
            (Payload::Float(a), Payload::Float(b)) => a == b,
            (Payload::Char(a), Payload::Char(b)) => a == b,
            (Payload::String(a), Payload::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Payload::Time(a), Payload::Time(b)) => a == b,
            (Payload::FilePath(a), Payload::FilePath(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Payload::Json(a), Payload::Json(b)) => a == b,
            (Payload::Array(a), Payload::Array(b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Payload::Tuple(a), Payload::Tuple(b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Payload::Object(a), Payload::Object(b)) => {
                a.len() == b.len() && Self::objects_equal(ctx, a, b)
            }
            (Payload::Map(a), Payload::Map(b)) => {
                a.len() == b.len() && Self::maps_equal(ctx, a, b)
            }
            (
                Payload::Tagged(ty1, idx1, p1),
                Payload::Tagged(ty2, idx2, p2),
            ) => {
                *ty1 == *ty2
                    && idx1 == idx2
                    && p1.len() == p2.len()
                    && Self::seqs_equal(ctx, p1.as_slice(), p2.as_slice())
            }
            (
                Payload::Ref(g1, name1, subs1),
                Payload::Ref(g2, name2, subs2),
            ) => {
                g1 == g2
                    && name1 == name2
                    && subs1.len() == subs2.len()
                    && Self::seqs_equal(ctx, subs1.as_slice(), subs2.as_slice())
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
        a: &IndexMap<MapKey, ValueId>,
        b: &IndexMap<MapKey, ValueId>,
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
    pub(crate) fn unwrap(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        match v {
            Payload::Tagged(ty, 1, p) if *ty == TypeId::OPTION => {
                let val = p
                    .first()
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("unwrap", "Option.Some payload")
                    });
                Ok(val)
            }
            Payload::Tagged(ty, 0, _) if *ty == TypeId::OPTION => {
                Err(Error::runtime(ctx.span, "cannot unwrap Option.None"))
            }
            Payload::Tagged(ty, 0, p) if *ty == TypeId::RESULT => {
                let val = p
                    .first()
                    .and_then(|id| ctx.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("unwrap", "Result.Ok payload")
                    });
                Ok(val)
            }
            Payload::Tagged(ty, 1, _) if *ty == TypeId::RESULT => {
                Err(Error::runtime(ctx.span, "cannot unwrap Result.Err"))
            }
            _ => typechecked!("unwrap", "Fallible"),
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
        v: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        let v_id =
            ctx.arena
                .add_typed(v.clone(), ValueMeta::untyped(), ctx.span);
        match target {
            Ty::Option(_) => Ok(Payload::some(v_id)),
            Ty::Result(_, _) => Ok(Payload::ok(v_id)),
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
        base: &Payload,
        idx: &Payload,
    ) -> Result<Payload> {
        match (base, idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
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
            (Payload::Map(entries), key) => entries
                .get(&Self::map_key(key))
                .and_then(|id| ctx.arena.get(*id).cloned())
                .ok_or_else(|| {
                    Error::runtime(
                        ctx.span,
                        format!("map key not found: {key:?}"),
                    )
                }),
            (Payload::String(sid), Payload::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars().nth(index as usize).map(Payload::Char).ok_or_else(
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
        base: &Payload,
        idx: &Payload,
    ) -> Result<Payload> {
        match (base, idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                Ok(index
                    .and_then(|idx| elems.get(idx))
                    .map(|id| Payload::some(*id))
                    .unwrap_or_else(Payload::none))
            }
            (Payload::Map(entries), key) => Ok(entries
                .get(&Self::map_key(key))
                .map(|id| Payload::some(*id))
                .unwrap_or_else(Payload::none)),
            (Payload::String(sid), Payload::Int(i)) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                Ok(s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        let id = ctx.arena.add_typed(
                            Payload::Char(c),
                            ctx.runtime_types.meta_char(),
                            ctx.span,
                        );
                        Payload::some(id)
                    })
                    .unwrap_or_else(Payload::none))
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
        val: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        match (val, target) {
            // Identity casts
            (Payload::Int(_), Ty::Int)
            | (Payload::Word(_), Ty::Word)
            | (Payload::Float(_), Ty::Float)
            | (Payload::Bool(_), Ty::Bool)
            | (Payload::Char(_), Ty::Char)
            | (Payload::String(_), Ty::String)
            | (Payload::FilePath(_), Ty::FilePath) => Ok(val.clone()),

            // Int -> Float (widen)
            (Payload::Int(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }

            // Word -> Int (always safe)
            (Payload::Word(n), Ty::Int) => Ok(Payload::Int(*n as i64)),

            // Word -> Float (widen)
            (Payload::Word(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }

            // Float -> Int (truncate)
            (Payload::Float(f), Ty::Int) => Ok(Payload::Int(f.0 as i64)),

            // Bool -> Int
            (Payload::Bool(b), Ty::Int) => {
                Ok(Payload::Int(if *b { 1 } else { 0 }))
            }

            // T -> String (stringify)
            (_, Ty::String) => {
                let s = Self::stringify(ctx, val);
                let id = ctx.arena.intern(&s);
                Ok(Payload::String(id))
            }

            // T -> Json (jsonify)
            (_, Ty::Json) => {
                Ok(Payload::Json(Arc::new(Self::jsonify(ctx, val))))
            }

            // String -> FilePath
            (Payload::String(sid), Ty::FilePath) => Ok(Payload::FilePath(*sid)),

            // DataStatus -> Int (variant idx to MUMPS value: 0, 1, 10, 11)
            (Payload::Tagged(ty, idx, _), Ty::Int)
                if *ty == TypeId::DATA_STATUS =>
            {
                let mumps_val = match idx {
                    0 => 0,  // NoData
                    1 => 1,  // HasValue
                    2 => 10, // HasDescendants
                    3 => 11, // Both
                    _ => typechecked!("DataStatus AS Int", "valid variant"),
                };
                Ok(Payload::Int(mumps_val))
            }

            // Path -> FilePath (extract filepath from either File or Dir variant)
            (Payload::Tagged(ty, _, payloads), Ty::FilePath)
                if *ty == TypeId::PATH =>
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
                Payload::Range {
                    start,
                    end,
                    inclusive,
                },
                Ty::Named(id, _),
            ) if *id == TypeId::ARRAY => {
                let end = if *inclusive { *end + 1 } else { *end };
                let elems = (*start..end)
                    .map(|n| {
                        ctx.arena.add_typed(
                            Payload::Int(n),
                            ctx.runtime_types.meta_int(),
                            ctx.span,
                        )
                    })
                    .collect();
                Ok(Payload::Array(Arc::new(elems)))
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
    fn is_storable_mismatch(val: &Payload, target: &Ty) -> bool {
        match (val, target) {
            (Payload::Bool(_), Ty::Bool)
            | (Payload::Int(_), Ty::Int)
            | (Payload::Float(_), Ty::Float)
            | (Payload::Char(_), Ty::Char)
            | (Payload::String(_), Ty::String)
            | (Payload::Json(_), Ty::Json) => false,
            // Payload is a Storable type but doesn't match target
            (
                Payload::Bool(_)
                | Payload::Int(_)
                | Payload::Float(_)
                | Payload::Char(_)
                | Payload::String(_)
                | Payload::Json(_),
                _,
            ) => true,
            // Not a Storable type at all; don't trigger this branch
            _ => false,
        }
    }

    /// Get a human-readable name for a value's type.
    fn value_type_name(ctx: &ClassCtx<'_>, val: &Payload) -> String {
        match val {
            Payload::Unit => "Unit".to_owned(),
            Payload::Bool(_) => "Bool".to_owned(),
            Payload::Int(_) => "Int".to_owned(),
            Payload::Word(_) => "Word".to_owned(),
            Payload::Float(_) => "Float".to_owned(),
            Payload::Char(_) => "Char".to_owned(),
            Payload::String(_) => "String".to_owned(),
            Payload::FilePath(_) => "FilePath".to_owned(),
            Payload::Json(_) => "Json".to_owned(),
            Payload::Array(_) => "Array".to_owned(),
            Payload::Tuple(_) => "Tuple".to_owned(),
            Payload::Object(_) => "Object".to_owned(),
            Payload::Map(_) => "Map".to_owned(),
            Payload::Time(_) => "Time".to_owned(),
            Payload::Regex(_) => "Regex".to_owned(),
            Payload::Range { .. } => "Range".to_owned(),
            Payload::Tagged(ty_id, _, _) => ctx
                .registry
                .type_name(*ty_id, ctx.arena)
                .unwrap_or("Tagged")
                .to_owned(),
            Payload::Closure { .. } => "Closure".to_owned(),
            Payload::Function { .. } => "Function".to_owned(),
            Payload::ModuleFn { .. } => "ModuleFn".to_owned(),
            Payload::ClassMethodFn { .. } => "ClassMethodFn".to_owned(),
            Payload::PartialApp { .. } => "PartialApp".to_owned(),
            Payload::ModuleConst { .. } => "ModuleConst".to_owned(),
            Payload::ForeverContinuation => "Continuation".to_owned(),
            Payload::LoopContinue(_) => "LoopContinue".to_owned(),
            Payload::Ref(is_global, _, _) => {
                if *is_global { "Global" } else { "Local" }.to_owned()
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
    fn coerce_to_str(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        match v {
            Payload::String(id) | Payload::FilePath(id) => {
                ctx.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => Display::format(ctx, v),
        }
    }

    /// Stringify a value for `AS String` conversion.
    fn stringify(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        Self::coerce_to_str(ctx, v)
    }

    /// Convert a value to JSON.
    ///
    /// Returns the JSON directly; for the class method wrapper that returns
    /// `Payload::Json`, dispatch to `Into[Json]` via `Into::into`.
    pub(crate) fn jsonify(
        ctx: &ClassCtx<'_>,
        v: &Payload,
    ) -> serde_json::Value {
        match v {
            Payload::Unit => serde_json::Value::Null,
            Payload::Bool(b) => serde_json::Value::Bool(*b),
            Payload::Int(n) => serde_json::json!(*n),
            Payload::Word(n) => serde_json::json!(*n),
            Payload::Float(f) => serde_json::json!(f.0),
            Payload::Char(c) => serde_json::Value::String(c.to_string()),
            Payload::String(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Payload::FilePath(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Payload::Array(arr) => {
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
            Payload::Tuple(elems) => {
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
            Payload::Object(obj) => {
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
            Payload::Tagged(ty_id, idx, payloads) => {
                // Option encodes as null/value rather than tagged object
                if *ty_id == TypeId::OPTION {
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
                    let ty_name = ctx
                        .registry
                        .type_name(*ty_id, ctx.arena)
                        .unwrap_or("?");
                    let var_name = ctx
                        .registry
                        .variant_name(*ty_id, *idx, ctx.arena)
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
            Payload::Map(entries) => {
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
            Payload::Time(t) => serde_json::Value::String(t.to_rfc3339()),
            Payload::Json(j) => j.as_ref().clone(),
            Payload::Regex(idx) => {
                let pattern = ctx
                    .regex_cache
                    .get(*idx as usize)
                    .map(|r| r.as_str())
                    .unwrap_or("?");
                serde_json::Value::String(pattern.to_owned())
            }
            Payload::Range {
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
            Payload::Closure { .. }
            | Payload::Function { .. }
            | Payload::ModuleFn { .. }
            | Payload::ClassMethodFn { .. }
            | Payload::ModuleConst { .. }
            | Payload::PartialApp { .. }
            | Payload::ForeverContinuation
            | Payload::LoopContinue(_) => serde_json::Value::Null,
            Payload::Ref(is_global, name_id, sub_ids) => {
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
        }
    }

    /// Convert a map key to a JSON-compatible string key.
    fn jsonify_map_key(ctx: &ClassCtx<'_>, k: &MapKey) -> String {
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
        val: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        match (val, target) {
            // Same type: identity conversion always succeeds
            _ if Self::types_match(val, target) => {
                Ok(Self::make_result_ok(ctx, val.clone()))
            }

            // String -> Int
            (Payload::String(sid), Ty::Int) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<i64>() {
                    Ok(n) => Self::make_result_ok(ctx, Payload::Int(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid integer: {s}"),
                    ),
                })
            }

            // String -> Float
            (Payload::String(sid), Ty::Float) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<f64>() {
                    Ok(n) => Self::make_result_ok(
                        ctx,
                        Payload::Float(OrderedFloat(n)),
                    ),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid float: {s}"),
                    ),
                })
            }

            // String -> Word
            (Payload::String(sid), Ty::Word) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<usize>() {
                    Ok(n) => Self::make_result_ok(ctx, Payload::Word(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid unsigned integer: {s}"),
                    ),
                })
            }

            // Int -> Bool (strict: only 0 and 1)
            (Payload::Int(n), Ty::Bool) => Ok(match *n {
                0 => Self::make_result_ok(ctx, Payload::Bool(false)),
                1 => Self::make_result_ok(ctx, Payload::Bool(true)),
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected 0 or 1 for Bool, got {n}"),
                ),
            }),

            // Int -> Word (must be non-negative)
            (Payload::Int(n), Ty::Word) => Ok(if *n >= 0 {
                Self::make_result_ok(ctx, Payload::Word(*n as usize))
            } else {
                Self::make_result_err(
                    ctx,
                    &format!("expected non-negative Int for Word, got {n}"),
                )
            }),

            // Json -> Bool
            (Payload::Json(j), Ty::Bool) => Ok(match &**j {
                serde_json::Value::Bool(b) => {
                    Self::make_result_ok(ctx, Payload::Bool(*b))
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
            (Payload::Json(j), Ty::Int) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_i64()
                    .map(|i| Self::make_result_ok(ctx, Payload::Int(i)))
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
            (Payload::Json(j), Ty::Float) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_f64()
                    .map(|f| {
                        Self::make_result_ok(
                            ctx,
                            Payload::Float(OrderedFloat(f)),
                        )
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
            (Payload::Json(j), Ty::String) => Ok(match &**j {
                serde_json::Value::String(s) => {
                    let id = ctx.arena.intern(s);
                    Self::make_result_ok(ctx, Payload::String(id))
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
                Payload::Json(Arc::new(Into::jsonify(ctx, val))),
            )),

            // Int -> DataStatus (MUMPS @data values: 0, 1, 10, 11 -> variants)
            (Payload::Int(n), Ty::DataStatus) => {
                let (variant_idx, valid) = match *n {
                    0 => (0, true),  // NoData
                    1 => (1, true),  // HasValue
                    10 => (2, true), // HasDescendants
                    11 => (3, true), // Both
                    _ => (0, false),
                };

                Ok(if valid {
                    Self::make_result_ok(
                        ctx,
                        Payload::Tagged(
                            TypeId::DATA_STATUS,
                            variant_idx,
                            SmallVec::new(),
                        ),
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
    fn types_match(val: &Payload, target: &Ty) -> bool {
        matches!(
            (val, target),
            (Payload::Bool(_), Ty::Bool)
                | (Payload::Int(_), Ty::Int)
                | (Payload::Word(_), Ty::Word)
                | (Payload::Float(_), Ty::Float)
                | (Payload::Char(_), Ty::Char)
                | (Payload::String(_), Ty::String)
                | (Payload::FilePath(_), Ty::FilePath)
                | (Payload::Json(_), Ty::Json)
                | (Payload::Unit, Ty::Unit)
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
    fn make_result_ok(ctx: &mut ClassCtx<'_>, val: Payload) -> Payload {
        let val_id = ctx.arena.add_typed(val, ValueMeta::untyped(), ctx.span);
        Payload::ok(val_id)
    }

    /// Create a `Result.Err(msg)` value.
    fn make_result_err(ctx: &mut ClassCtx<'_>, msg: &str) -> Payload {
        let msg_id = ctx.arena.intern(msg);
        let msg_val_id = ctx.arena.add_typed(
            Payload::String(msg_id),
            ctx.runtime_types.meta_string(),
            ctx.span,
        );
        Payload::err(msg_val_id)
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
    pub(crate) fn display(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        let s = Self::format(ctx, v);
        let id = ctx.arena.intern(&s);
        Ok(Payload::String(id))
    }

    /// Format a value as valid RUMPS syntax.
    ///
    /// Returns the string directly; for the class method wrapper that returns
    /// `Payload::String`, see [`display`](Self::display).
    pub(crate) fn format(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        match v {
            Payload::Unit => "Unit".into(),
            Payload::Bool(b) => b.to_string(),
            Payload::Int(n) => n.to_string(),
            Payload::Word(n) => n.to_string(),
            Payload::Float(f) => {
                let s = f.to_string();
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
            Payload::Char(c) => format!("'{c}'"),
            Payload::String(id) | Payload::FilePath(id) => {
                let s = ctx.arena.get_str(*id).unwrap_or("");
                format!("\"{s}\"")
            }
            Payload::Regex(idx) => {
                let re =
                    ctx.regex_cache.get(*idx as usize).unwrap_or_else(|| {
                        typechecked!("Display", "valid Regex cache index")
                    });
                format!("/{}/", re.as_str())
            }
            Payload::Array(elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .join(", ");
                format!("[ {items} ]")
            }
            Payload::Tuple(elems) => {
                let items = elems
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .join(", ");
                let trail = if elems.len() == 1 { "," } else { "" };
                format!("({items}{trail})")
            }
            Payload::Object(obj) => {
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
                    .join(", ");
                format!("{{ {fields} }}")
            }
            Payload::Map(entries) => {
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
                    .join(", ");
                format!("{{ {items} }}")
            }
            Payload::Time(t) => t.to_rfc3339(),
            Payload::Json(j) => j.to_string(),
            Payload::Tagged(ty_id, idx, payloads) => {
                let ty_name =
                    ctx.registry.type_name(*ty_id, ctx.arena).unwrap_or("?");
                let var_name = ctx
                    .registry
                    .variant_name(*ty_id, *idx, ctx.arena)
                    .unwrap_or("?");

                if payloads.is_empty() {
                    format!("{ty_name}.{var_name}")
                } else {
                    let args = payloads
                        .iter()
                        .filter_map(|id| ctx.arena.get(*id))
                        .map(|v| Self::format(ctx, v))
                        .join(", ");
                    format!("{ty_name}.{var_name}({args})")
                }
            }
            Payload::Closure { .. } => {
                typechecked!("Display", "Display (not Closure)")
            }
            Payload::Function { .. } => {
                typechecked!("Display", "Display (not Function)")
            }
            Payload::ModuleFn { .. } => {
                typechecked!("Display", "Display (not ModuleFn)")
            }
            Payload::ClassMethodFn { .. } => {
                typechecked!("Display", "Display (not ClassMethodFn)")
            }
            Payload::PartialApp { .. } => {
                typechecked!("Display", "Display (not PartialApp)")
            }
            Payload::ModuleConst { path } => {
                let path_str: String = path
                    .iter()
                    .filter_map(|id| ctx.arena.get_str(*id))
                    .join(".");
                format!("<{path_str}>")
            }
            Payload::Range {
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
            Payload::ForeverContinuation => "<continuation>".into(),
            Payload::LoopContinue(_) => "<loop-continue>".into(),
            Payload::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = ctx.arena.get_str(*name_id).unwrap_or("?");
                let subs = sub_ids
                    .iter()
                    .filter_map(|id| ctx.arena.get(*id))
                    .map(|v| Self::format(ctx, v))
                    .join(", ");
                format!("{prefix}{name}{{{subs}}}")
            }
        }
    }

    /// Format a map key for display.
    fn format_map_key(ctx: &ClassCtx<'_>, k: &MapKey) -> String {
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
            OptionNone,
            ResultOk(ValueId),
            ResultErr(Payload),
            Other,
        }
        let kind = match ctx.arena.get(src) {
            Some(Payload::Array(elems)) if elems.is_empty() => Kind::EmptyArray,
            Some(Payload::Array(elems)) => Kind::Array(elems[0]),
            // Option.Some(v) -> map inner
            Some(Payload::Tagged(ty, 1, payloads)) if *ty == TypeId::OPTION => {
                Kind::OptionSome(
                    *payloads
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            // Option.None -> return None
            Some(Payload::Tagged(ty, 0, _)) if *ty == TypeId::OPTION => {
                Kind::OptionNone
            }
            // Result.Ok(v) -> map inner
            Some(Payload::Tagged(ty, 0, payloads)) if *ty == TypeId::RESULT => {
                let inner = *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Ok has payload"));
                Kind::ResultOk(inner)
            }
            // Result.Err(e) -> return unchanged
            Some(v @ Payload::Tagged(ty, 1, _)) if *ty == TypeId::RESULT => {
                Kind::ResultErr(v.clone())
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::EmptyArray => Ok(MethodResult::Done(Payload::Array(
                Arc::new(SmallVec::new()),
            ))),
            Kind::Array(first) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![first],
                state: HofState::MapIter {
                    kind: IterKind::Array {
                        source: src,
                        idx: 0,
                    },
                    acc: SmallVec::new(),
                },
            })),
            Kind::OptionSome(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: HofState::MapContainer {
                    ctor_ty: TypeId::OPTION,
                    tag: 1, // Some
                },
            })),
            Kind::OptionNone => Ok(MethodResult::Done(Payload::none())),
            Kind::ResultOk(inner) => {
                Ok(MethodResult::Invoke(Continuation {
                    callee: fn_id,
                    args: smallvec![inner],
                    state: HofState::MapContainer {
                        ctor_ty: TypeId::RESULT,
                        tag: 0, // Ok
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

        match ctx.arena.get(src) {
            Some(Payload::Array(elems)) if elems.is_empty() => Ok(
                MethodResult::Done(Payload::Array(Arc::new(SmallVec::new()))),
            ),
            Some(Payload::Array(elems)) => {
                let first = elems[0];
                Ok(MethodResult::Invoke(Continuation {
                    callee: pred_id,
                    args: smallvec![first],
                    state: HofState::FilterArray {
                        source: src,
                        idx: 0,
                        acc: SmallVec::new(),
                        pending: first,
                    },
                }))
            }
            _ => typechecked!("Filterable:filter", "Array"),
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
        let kind = match ctx.arena.get(src) {
            Some(Payload::Array(elems)) if elems.is_empty() => Kind::EmptyArray,
            Some(Payload::Array(elems)) => Kind::Array(elems[0]),
            Some(Payload::Range {
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
                let int_id = ctx.arena.add_typed(
                    Payload::Int(start),
                    ctx.runtime_types.meta_int(),
                    ctx.span,
                );
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
    pub(crate) fn length(_: &mut ClassCtx<'_>, v: &Payload) -> Result<Payload> {
        Ok(match v {
            Payload::Array(elems) => Payload::Int(elems.len() as i64),
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                let len = if *inclusive {
                    end - start + 1
                } else {
                    end - start
                };
                Payload::Int(len.max(0))
            }
            _ => typechecked!("Iterable:length", "Iterable"),
        })
    }

    /// `Iterable:collect`; materializes an iterable into an `Array`.
    pub(crate) fn collect(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        Ok(match v {
            Payload::Array(elems) => Payload::Array(elems.clone()),
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                let actual_end = if *inclusive { *end + 1 } else { *end };
                let elems: SmallVec<[ValueId; 4]> = (*start..actual_end)
                    .map(|i| {
                        ctx.arena.add_typed(
                            Payload::Int(i),
                            ctx.runtime_types.meta_int(),
                            ctx.span,
                        )
                    })
                    .collect();
                Payload::Array(Arc::new(elems))
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
            Some(Payload::Tagged(ty, 0, _)) if *ty == TypeId::OPTION => {
                Ok(MethodResult::Done(Payload::none()))
            }
            // Option.Some(v) -> invoke fn(v)
            Some(Payload::Tagged(ty, 1, payloads)) if *ty == TypeId::OPTION => {
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
            Some(Payload::Tagged(ty, 0, payloads)) if *ty == TypeId::RESULT => {
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
            Some(Payload::Tagged(ty, 1, payloads)) if *ty == TypeId::RESULT => {
                let err = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                Ok(MethodResult::Done(Payload::Tagged(*ty, 1, smallvec![err])))
            }
            _ => typechecked!("Chainable:chain", "Option or Result"),
        }
    }
}

/// `Bimappable` class: `bimap` method.
pub(crate) struct Bimappable;

impl Bimappable {
    pub(crate) fn bimap(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<MethodResult> {
        let f = *args
            .first()
            .unwrap_or_else(|| typechecked!("Bimappable:bimap", "3 args"));
        let g = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Bimappable:bimap", "3 args"));
        let src = *args
            .get(2)
            .unwrap_or_else(|| typechecked!("Bimappable:bimap", "3 args"));

        enum Kind {
            ResultOk(ValueId),
            ResultErr(ValueId),
            Tuple(ValueId, ValueId),
            Other,
        }

        let kind = match ctx.arena.get(src) {
            Some(Payload::Tagged(ty, 0, payloads)) if *ty == TypeId::RESULT => {
                Kind::ResultOk(
                    *payloads
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                )
            }
            Some(Payload::Tagged(ty, 1, payloads)) if *ty == TypeId::RESULT => {
                Kind::ResultErr(
                    *payloads
                        .first()
                        .unwrap_or_else(|| invariant!("Err has payload")),
                )
            }
            Some(Payload::Tuple(elems)) => {
                let a = *elems
                    .first()
                    .unwrap_or_else(|| invariant!("bimap tuple has 2 elems"));
                let b = *elems
                    .get(1)
                    .unwrap_or_else(|| invariant!("bimap tuple has 2 elems"));
                Kind::Tuple(a, b)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::ResultOk(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: f,
                args: smallvec![inner],
                state: HofState::BimapResult { tag: 0 },
            })),
            Kind::ResultErr(inner) => Ok(MethodResult::Invoke(Continuation {
                callee: g,
                args: smallvec![inner],
                state: HofState::BimapResult { tag: 1 },
            })),
            Kind::Tuple(a, b) => Ok(MethodResult::Invoke(Continuation {
                callee: f,
                args: smallvec![a],
                state: HofState::BimapTuple {
                    second_fn: g,
                    second_elem: b,
                    first_result: None,
                },
            })),
            Kind::Other => typechecked!("Bimappable:bimap", "Bimappable"),
        }
    }
}
