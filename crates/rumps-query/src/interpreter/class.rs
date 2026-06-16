//! Class method dispatch infrastructure.
//!
//! Provides a registry for class methods (like `Additive:add`, `Fallible:unwrap`)
//! and dispatch functions to invoke them. The dispatch table is indexed by
//! `ClassId` for O(1) lookup.
//!
//! # Organization
//!
//! Each type class is represented by a unit struct implementing `Class`:
//! - `Numeric`: marker class
//! - `Additive`: `zero`, `add`
//! - `Subtractive`: `sub`
//! - `Multiplicative`: `one`, `mul`
//! - `Divisible`: `div`
//! - `FloorDivisible`: `floor-div`, `mod`
//! - `Powerable`: `pow`
//! - `Negatable`: `neg`
//! - `BitLike`: `bit-and`, `bit-or`, `shl`, `shr`
//! - `Default`: `default`
//! - `Concatable`: `concat`
//! - `Ord`: `compare`
//! - `Eq`: `eq`
//! - `Fallible`: `unwrap`
//! - `Wrappable`: `wrap`
//! - `Chainable`: `chain`
//! - `Indexable`: `index`, `get`
//! - `Mappable`: `map`
//! - `Filterable`: `filter`
//! - `Foldable`: `fold`, `fold-map`
//! - `Iterable`: `length`, `reverse`
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

use super::hof;
use crate::intern::{StringId, StringInterner};
use crate::primitives::Range;
use crate::typecheck::{RuntimeTyId, RuntimeTypes, Ty, TyArena};
use crate::value::{
    Map, Payload, TypeId, TypeRegistry, Value, ValueArena, ValueId,
};
use crate::{ClassId, Error, Result, Span};

/// Context for class method dispatch.
pub(crate) struct ClassCtx<'a> {
    pub(crate) arena: &'a mut ValueArena,
    pub(crate) runtime_types: &'a mut RuntimeTypes,
    pub(crate) registry: &'a TypeRegistry,
    pub(crate) regex_cache: &'a [regex::Regex],
    pub(crate) span: Span,
}

impl ClassCtx<'_> {
    pub(crate) fn add(&mut self, v: Payload) -> ValueId {
        let meta = self.runtime_types.meta_for_payload(self.arena, &v);
        self.arena.add_typed(v, meta, self.span)
    }

    pub(crate) fn option_some(&mut self, v: ValueId) -> ValueId {
        let elem = self
            .arena
            .meta(v)
            .map(|m| m.ty)
            .unwrap_or_else(|| RuntimeTyId::from(TyArena::UNIT));
        let ty = self.runtime_types.option(elem);
        self.arena.add_typed(
            Payload::some(v),
            self.runtime_types.meta(ty),
            self.span,
        )
    }

    pub(crate) fn option_none(&mut self) -> ValueId {
        let ty = self.runtime_types.option(RuntimeTyId::from(TyArena::UNIT));
        self.arena.add_typed(
            Payload::none(),
            self.runtime_types.meta(ty),
            self.span,
        )
    }

    fn value_base_type(&self, id: ValueId) -> Option<TypeId> {
        self.arena.meta(id).and_then(|meta| {
            self.runtime_types
                .to_type_id(meta.repr)
                .or_else(|| self.runtime_types.to_type_id(meta.ty))
        })
    }

    fn runtime_base_type(&self, ty: RuntimeTyId) -> Option<TypeId> {
        self.runtime_types.to_type_id(ty)
    }

    fn value_variant_base_type(&self, value: &Value) -> Option<TypeId> {
        self.runtime_base_type(value.repr)
            .or_else(|| self.runtime_base_type(value.ty))
    }
}

/// Binary class method signature.
pub(crate) type BinMethodFn =
    fn(&mut ClassCtx<'_>, &Payload, &Payload) -> Result<Payload>;

/// Unary class method signature.
pub(crate) type UnaryMethodFn =
    fn(&mut ClassCtx<'_>, &Payload) -> Result<Payload>;

/// Nullary class method signature, e.g. `Default:default`.
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
    Hof(hof::MethodFn),
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
        self.tables.get(kind.idx()).and_then(|t| t.lookup(name))
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
        // Keep this list in `ClassId` order. `ClassMethods` indexes method
        // tables by `ClassId`, while each class module owns its own method
        // names and method function registration. Marker classes still appear
        // here so empty runtime registration is explicit.
        Numeric::register_all(self, i);
        Additive::register_all(self, i);
        Subtractive::register_all(self, i);
        Multiplicative::register_all(self, i);
        Divisible::register_all(self, i);
        FloorDivisible::register_all(self, i);
        Powerable::register_all(self, i);
        Negatable::register_all(self, i);
        BitLike::register_all(self, i);
        Ord::register_all(self, i);
        Eq::register_all(self, i);
        DefaultClass::register_all(self, i);
        Concatable::register_all(self, i);
        Fallible::register_all(self, i);
        Wrappable::register_all(self, i);
        Indexable::register_all(self, i);
        Into::register_all(self, i);
        TryInto::register_all(self, i);
        Display::register_all(self, i);
        Mappable::register_all(self, i);
        Filterable::register_all(self, i);
        Foldable::register_all(self, i);
        Iterable::register_all(self, i);
        Chainable::register_all(self, i);
        Bimappable::register_all(self, i);
    }
}

impl Default for ClassMethods {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared utilities for class method implementations.
pub(crate) trait Class {
    const ID: ClassId;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner);

    fn register(
        methods: &mut ClassMethods,
        i: &mut StringInterner,
        name: &str,
        f: MethodFn,
    ) {
        methods.register(Self::ID, i.intern(name), f);
    }
}

mod additive;
mod bimappable;
mod bitlike;
mod chainable;
mod concatable;
mod default;
mod display;
mod divisible;
mod eq;
mod fallible;
mod filterable;
mod floor_divisible;
mod foldable;
mod indexable;
mod into;
mod iterable;
mod mappable;
mod multiplicative;
mod negatable;
mod numeric;
mod ord;
mod powerable;
mod subtractive;
mod try_into;
mod wrappable;

pub(crate) use additive::Additive;
pub(crate) use bimappable::Bimappable;
pub(crate) use bitlike::BitLike;
pub(crate) use chainable::Chainable;
pub(crate) use concatable::Concatable;
pub(crate) use default::DefaultClass;
pub(crate) use display::Display;
pub(crate) use divisible::Divisible;
pub(crate) use eq::Eq;
pub(crate) use fallible::Fallible;
pub(crate) use filterable::Filterable;
pub(crate) use floor_divisible::FloorDivisible;
pub(crate) use foldable::Foldable;
pub(crate) use indexable::Indexable;
pub(crate) use into::Into;
pub(crate) use iterable::Iterable;
pub(crate) use mappable::Mappable;
pub(crate) use multiplicative::Multiplicative;
pub(crate) use negatable::Negatable;
pub(crate) use numeric::Numeric;
pub(crate) use ord::Ord;
pub(crate) use powerable::Powerable;
pub(crate) use subtractive::Subtractive;
pub(crate) use try_into::TryInto;
pub(crate) use wrappable::Wrappable;
