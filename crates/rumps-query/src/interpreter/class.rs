//! Class method target registry.
//!
//! Provides a registry for class methods like `Additive:add` and
//! `Fallible:unwrap`. The dispatch table is indexed by `ClassId` for `O(1)`
//! lookup.
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
//! [`Interpreter`]: super::Interpreter

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) use dispatch::Dispatch;
use indexmap::IndexMap;
use itertools::Itertools;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use super::Interpreter;
use crate::builtins::{self, BuiltinCtx, Impl, Range, Values};
use crate::intern::{StringId, StringInterner};
use crate::typecheck::{RuntimeTyId, Ty, TyArena};
use crate::value::{Map, Payload, TypeId, Value, ValueId};
use crate::{ClassId, Error, Result, Span};

#[derive(Clone, Copy)]
pub(crate) struct MethodDef {
    pub(crate) abi: MethodAbi,
    pub(crate) builtin: Builtin,
}

#[derive(Clone, Copy)]
pub(crate) enum MethodAbi {
    Binary,
    Unary,
    Nullary,
    Convert,
    Hkt,
}

#[derive(Clone, Copy)]
pub(crate) enum Builtin {
    Fixed(builtins::Impl),
    Selected(SelectFn),
}

pub(crate) type SelectFn = for<'i, 'ast, 'io> fn(
    &'i mut Interpreter<'ast, 'io>,
    &Dispatch,
) -> Result<builtins::Call>;

/// Per-class method table.
struct MethodTable {
    methods: HashMap<StringId, MethodDef>,
}

impl MethodTable {
    fn new() -> Self {
        Self {
            methods: HashMap::new(),
        }
    }

    fn register(&mut self, name: StringId, def: MethodDef) {
        self.methods.insert(name, def);
    }

    fn lookup(&self, name: StringId) -> Option<&MethodDef> {
        self.methods.get(&name)
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
        def: MethodDef,
    ) {
        self.tables[kind.idx()].register(name, def);
    }

    pub(crate) fn lookup(
        &self,
        kind: ClassId,
        name: StringId,
    ) -> Option<&MethodDef> {
        self.tables.get(kind.idx()).and_then(|t| t.lookup(name))
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
        abi: MethodAbi,
        target: Builtin,
    ) {
        methods.register(
            Self::ID,
            i.intern(name),
            MethodDef {
                abi,
                builtin: target,
            },
        );
    }
}

mod additive;
mod bimappable;
mod bitlike;
mod chainable;
mod concatable;
mod default;
mod dispatch;
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
