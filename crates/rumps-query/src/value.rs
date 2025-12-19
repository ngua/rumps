//! Runtime value types with arena allocation and string interning.
//!
//! Uses arena allocation for cache efficiency and to avoid `Box` in recursive
//! structures. Strings are interned to avoid duplication and enable O(1)
//! comparison. A type registry enables runtime type validation, `is` checks,
//! and clear error messages.
//!
//! Note: Value conversion methods (to/from storage, JSON, display) live on
//! `Interpreter` rather than `Value` because they require context (arena,
//! registry) that the interpreter owns.

#![allow(dead_code)]

use std::collections::HashMap;

use indexmap::{IndexMap, IndexSet};
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use crate::ast::ExprId;
use crate::{Result, Span};

/// Index into the value arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct ValueId(u32);

impl ValueId {
    const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Index into the string intern table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct StringId(u32);

impl StringId {
    fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Index into the type registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct TypeId(u32);

impl TypeId {
    /// Builtin type: `Bool`.
    pub(crate) const BOOL: Self = Self(0);
    /// Builtin type: `Int`.
    pub(crate) const INT: Self = Self(1);
    /// Builtin type: `Float`.
    pub(crate) const FLOAT: Self = Self(2);
    /// Builtin type: `String`.
    pub(crate) const STRING: Self = Self(3);
    /// Builtin type: `Array`.
    pub(crate) const ARRAY: Self = Self(4);
    /// Builtin type: `Object`.
    pub(crate) const OBJECT: Self = Self(5);
    /// Builtin type: `Option`.
    pub(crate) const OPTION: Self = Self(6);
    /// Builtin type: `Result`.
    pub(crate) const RESULT: Self = Self(7);
    /// Builtin type: `Char`.
    pub(crate) const CHAR: Self = Self(8);
    /// Placeholder type for uninferred type parameters; compatible with any type.
    /// Used for empty arrays (unknown element type) and partial variant types
    /// (e.g., `Option.None` has unknown `T`, `Result.Ok(v)` has unknown `E`).
    pub(crate) const UNKNOWN: Self = Self(9);

    const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Arena for runtime values with string interning.
///
/// Values are stored contiguously for cache efficiency. Spans are stored in
/// parallel for error messages (e.g., "type mismatch: value created at line 5").
#[derive(Clone, Debug)]
pub(crate) struct ValueArena {
    values: Vec<Value>,
    value_spans: Vec<Span>,
    strings: IndexSet<String>,
}

impl Default for ValueArena {
    fn default() -> Self {
        Self::new()
    }
}

impl ValueArena {
    /// Create an empty value arena.
    pub(crate) fn new() -> Self {
        Self {
            values: Vec::new(),
            value_spans: Vec::new(),
            strings: IndexSet::new(),
        }
    }

    /// Add a value to the arena.
    pub(crate) fn add(&mut self, v: Value, span: Span) -> ValueId {
        let id = ValueId(self.values.len() as u32);
        self.values.push(v);
        self.value_spans.push(span);
        id
    }

    /// Get a value by ID.
    pub(crate) fn get(&self, id: ValueId) -> Option<&Value> {
        self.values.get(id.idx())
    }

    /// Get the span of a value.
    pub(crate) fn span(&self, id: ValueId) -> Option<Span> {
        self.value_spans.get(id.idx()).copied()
    }

    /// Intern a string, returning its ID.
    ///
    /// If the string is already interned, returns the existing ID.
    pub(crate) fn intern(&mut self, s: &str) -> StringId {
        self.strings.get_index_of(s).map_or_else(
            || {
                let (idx, _) = self.strings.insert_full(s.to_owned());
                StringId(idx as u32)
            },
            |idx| StringId(idx as u32),
        )
    }

    /// Get a string by its interned ID.
    pub(crate) fn get_str(&self, id: StringId) -> Option<&str> {
        self.strings.get_index(id.idx()).map(String::as_str)
    }

    /// Look up a string's ID without interning it.
    ///
    /// Returns `None` if the string has not been interned.
    pub(crate) fn lookup_string(&self, s: &str) -> Option<StringId> {
        self.strings.get_index_of(s).map(|idx| StringId(idx as u32))
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn string_count(&self) -> usize {
        self.strings.len()
    }
}

/// Captured lexical environment for closures.
///
/// When a closure is created, it captures the current lexical scope by value.
/// This flattened map contains all bindings accessible at capture time.
/// Since closures capture by value (not reference), later rebindings of the
/// same name in the outer scope do not affect the captured value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CapturedEnv {
    bindings: HashMap<StringId, ValueId>,
}

impl CapturedEnv {
    /// Create a captured environment from a snapshot of the current scope stack.
    ///
    /// Flattens all visible bindings into a single map. If the same name appears
    /// in multiple scopes, the innermost (most recent) binding wins (since
    /// `collect()` into `HashMap` keeps the last value for duplicate keys).
    pub(crate) fn capture(scopes: &[HashMap<StringId, ValueId>]) -> Self {
        let bindings = scopes
            .iter()
            .flat_map(|frame| frame.iter())
            .map(|(k, v)| (*k, *v))
            .collect();
        Self { bindings }
    }

    /// Look up a name in the captured environment.
    pub(crate) fn lookup(&self, name: StringId) -> Option<ValueId> {
        self.bindings.get(&name).copied()
    }
}

/// A runtime value.
///
/// Uses `StringId` for interned strings and `ValueId` for nested values,
/// avoiding allocation and enabling O(1) string comparison.
///
/// Note: `Ord` and `Hash` are not derived because `Object` contains `HashMap`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// A boolean value.
    Bool(bool),

    /// A 64-bit integer.
    Int(i64),

    /// A 64-bit floating-point number (ordered via `OrderedFloat`;
    /// NaN cannot be represented).
    Float(OrderedFloat<f64>),

    /// A single UTF-8 character.
    Char(char),

    /// An interned string.
    String(StringId),

    /// An array of values with element type. Note that in native RUMPS arrays
    /// elements must by homogeneous
    Array(TypeExprId, SmallVec<[ValueId; 4]>),

    /// An object/record with string keys (insertion order preserved).
    Object(IndexMap<StringId, ValueId>),

    /// A tagged value (sum type variant).
    ///
    /// - `TypeExprId`: the full parameterized type (e.g., `Option[Int]`, `Result[Int, String]`)
    /// - `u8`: the variant index (e.g., `0` for `None`, `1` for `Some`)
    /// - `SmallVec`: the payload values (most variants have 0-4)
    Tagged(TypeExprId, u8, SmallVec<[ValueId; 4]>),

    /// A closure (anonymous function) with captured environment.
    ///
    /// Closures capture their lexical scope at creation time by value.
    /// The body is an AST expression ID; the interpreter evaluates it
    /// with the captured environment restored when the closure is called.
    Closure {
        params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
        ret: Option<TypeExprId>,
        body: ExprId,
        env: CapturedEnv,
    },
}

impl Value {
    /// Check if this value is truthy.
    ///
    /// Falsy values: `false`, `0`, `0.0`, `""`, `[]`, `{}`, `Option.None`, `Result.Err`
    pub(crate) fn is_truthy(
        &self,
        arena: &ValueArena,
        type_exprs: &TypeExprArena,
    ) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Int(n) => *n != 0,
            Self::Float(f) => f.0 != 0.0,
            Self::Char(c) => *c != '\0',
            Self::String(id) => {
                arena.get_str(*id).map(|s| !s.is_empty()).unwrap_or(false)
            }
            Self::Array(_, elems) => !elems.is_empty(),
            Self::Object(obj) => !obj.is_empty(),
            Self::Tagged(ty_expr, idx, _) => {
                // Option.None and Result.Err are falsy; other variants are truthy
                match type_exprs.base_type(*ty_expr) {
                    Some(TypeId::OPTION) => *idx != 0, // Some is truthy
                    Some(TypeId::RESULT) => *idx == 0, // Ok is truthy
                    _ => true, // Unknown tagged → truthy
                }
            }
            // Closures are always truthy (like functions in most languages)
            Self::Closure { .. } => true,
        }
    }

    /// Get the type name of this value for error messages.
    pub(crate) fn type_name(
        &self,
        reg: &TypeRegistry,
        type_exprs: &TypeExprArena,
    ) -> &'static str {
        match self {
            Self::Bool(_) => "Bool",
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::Char(_) => "Char",
            Self::String(_) => "String",
            Self::Array(..) => "Array",
            Self::Object(_) => "Object",
            Self::Tagged(ty_expr, _, _) => type_exprs
                .base_type(*ty_expr)
                .and_then(|ty| reg.get_def(ty))
                .map(|def| match def {
                    TypeDef::Builtin(b) => b.name(),
                    TypeDef::Sum { .. } => "Tagged",
                })
                .unwrap_or("Unknown"),
            Self::Closure { .. } => "Closure",
        }
    }

    /// Create an `Option.None` value with the given type expression.
    pub(crate) fn none(ty_expr: TypeExprId) -> Self {
        Self::Tagged(ty_expr, 0, SmallVec::new())
    }

    /// Create an `Option.Some(v)` value with the given type expression.
    pub(crate) fn some(ty_expr: TypeExprId, v: ValueId) -> Self {
        Self::Tagged(ty_expr, 1, smallvec::smallvec![v])
    }

    /// Create a `Result.Ok(v)` value with the given type expression.
    pub(crate) fn ok(ty_expr: TypeExprId, v: ValueId) -> Self {
        Self::Tagged(ty_expr, 0, smallvec::smallvec![v])
    }

    /// Create a `Result.Err(e)` value with the given type expression.
    pub(crate) fn err(ty_expr: TypeExprId, e: ValueId) -> Self {
        Self::Tagged(ty_expr, 1, smallvec::smallvec![e])
    }

    /// Check if this is `Option.None`.
    pub(crate) fn is_none(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 0, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::OPTION),
            _ => false,
        }
    }

    /// Check if this is `Option.Some`.
    pub(crate) fn is_some(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 1, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::OPTION),
            _ => false,
        }
    }

    /// Check if this is `Result.Ok`.
    pub(crate) fn is_ok(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 0, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::RESULT),
            _ => false,
        }
    }

    /// Check if this is `Result.Err`.
    pub(crate) fn is_err(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 1, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::RESULT),
            _ => false,
        }
    }
}

/// Built-in primitive types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinType {
    Bool,
    Int,
    Float,
    Char,
    String,
    Array,
    Object,
}

impl BuiltinType {
    const fn name(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Int => "Int",
            Self::Float => "Float",
            Self::Char => "Char",
            Self::String => "String",
            Self::Array => "Array",
            Self::Object => "Object",
        }
    }
}

/// A variant definition for sum types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VariantDef {
    pub(crate) name: StringId,
    pub(crate) idx: u8,
    pub(crate) arity: u8,
}

/// A type definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TypeDef {
    Builtin(BuiltinType),
    Sum {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        variants: SmallVec<[VariantDef; 4]>,
    },
}

/// Index into the type expression arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct TypeExprId(u32);

impl TypeExprId {
    const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// A type expression for annotations (not stored in values; used for validation).
///
/// Examples: `Int`, `Array[String]`, `Result[Int, String]`, `(Int) -> Int`
#[derive(Clone, Debug, PartialEq, Eq)]
enum TypeExpr {
    Named(TypeId),
    App(TypeId, SmallVec<[TypeExprId; 2]>),
    /// Function type: `(params...) -> return`
    Fn(SmallVec<[TypeExprId; 4]>, TypeExprId),
}

/// Arena for type expressions.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeExprArena {
    exprs: Vec<TypeExpr>,
}

impl TypeExprArena {
    pub(crate) fn new() -> Self {
        Self { exprs: Vec::new() }
    }

    fn add(&mut self, expr: TypeExpr) -> TypeExprId {
        let id = TypeExprId(self.exprs.len() as u32);
        self.exprs.push(expr);
        id
    }

    fn get(&self, id: TypeExprId) -> Option<&TypeExpr> {
        self.exprs.get(id.idx())
    }

    /// Get the base `TypeId` from a type expression.
    ///
    /// For `Named(T)` returns `T`; for `App(T, params)` returns `T`.
    /// For `Fn` returns `None` (function types have no base type).
    pub(crate) fn base_type(&self, id: TypeExprId) -> Option<TypeId> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Named(ty) | TypeExpr::App(ty, _) => Some(*ty),
            TypeExpr::Fn(..) => None,
        })
    }

    /// Add a simple named type expression.
    pub(crate) fn named(&mut self, ty: TypeId) -> TypeExprId {
        self.add(TypeExpr::Named(ty))
    }

    /// Add a parameterized type expression (e.g., `Result[Int, String]`).
    pub(crate) fn app(
        &mut self,
        ty: TypeId,
        params: SmallVec<[TypeExprId; 2]>,
    ) -> TypeExprId {
        self.add(TypeExpr::App(ty, params))
    }

    /// Check if two type expressions are structurally equal.
    pub(crate) fn eq(&self, a: TypeExprId, b: TypeExprId) -> bool {
        self.get(a)
            .zip(self.get(b))
            .is_some_and(|(ta, tb)| self.exprs_eq(ta, tb))
    }

    /// Structural equality of type expressions.
    ///
    /// `UNKNOWN` is compatible with any type (used for uninferred type params).
    fn exprs_eq(&self, a: &TypeExpr, b: &TypeExpr) -> bool {
        match (a, b) {
            // UNKNOWN is compatible with anything (uninferred type parameter)
            (TypeExpr::Named(TypeId::UNKNOWN), _)
            | (_, TypeExpr::Named(TypeId::UNKNOWN)) => true,
            (TypeExpr::Named(ta), TypeExpr::Named(tb)) => ta == tb,
            (TypeExpr::App(ta, pa), TypeExpr::App(tb, pb)) => {
                ta == tb
                    && pa.len() == pb.len()
                    && pa.iter().zip(pb.iter()).all(|(a, b)| self.eq(*a, *b))
            }
            (TypeExpr::Fn(pa, ra), TypeExpr::Fn(pb, rb)) => {
                pa.len() == pb.len()
                    && pa.iter().zip(pb.iter()).all(|(a, b)| self.eq(*a, *b))
                    && self.eq(*ra, *rb)
            }
            _ => false,
        }
    }

    /// Format a type expression for display.
    ///
    /// The `name_fn` closure converts `TypeId` to a name string.
    pub(crate) fn format<F>(&self, id: TypeExprId, name_fn: F) -> Option<String>
    where
        F: Fn(TypeId) -> String + Copy,
    {
        self.get(id).map(|expr| self.format_expr(expr, name_fn))
    }

    fn format_expr<F>(&self, expr: &TypeExpr, name_fn: F) -> String
    where
        F: Fn(TypeId) -> String + Copy,
    {
        match expr {
            TypeExpr::Named(ty) => name_fn(*ty),
            TypeExpr::App(ty, params) => {
                let name = name_fn(*ty);
                let args = params
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}[{args}]")
            }
            TypeExpr::Fn(params, ret) => {
                let args = params
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn))
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_str = self
                    .format(*ret, name_fn)
                    .unwrap_or_else(|| "?".to_owned());
                format!("({args}) -> {ret_str}")
            }
        }
    }

    /// Add a function type expression (e.g., `(Int, Int) -> Int`).
    pub(crate) fn fn_type(
        &mut self,
        params: SmallVec<[TypeExprId; 4]>,
        ret: TypeExprId,
    ) -> TypeExprId {
        self.add(TypeExpr::Fn(params, ret))
    }
}

/// Registry of all type definitions.
///
/// Enables runtime type validation, `is` checks, and clear error messages.
/// Builtins are registered at construction; `TypeId::OPTION` and `TypeId::RESULT`
/// are reserved at indices 6 and 7.
#[derive(Clone, Debug)]
pub(crate) struct TypeRegistry {
    defs: Vec<TypeDef>,
    by_name: HashMap<StringId, TypeId>,
}

impl TypeRegistry {
    /// Create a type registry with all builtins registered.
    pub(crate) fn new(arena: &mut ValueArena) -> Result<Self> {
        let mut reg = Self {
            defs: Vec::new(),
            by_name: HashMap::new(),
        };
        reg.register_builtins(arena)?;
        Ok(reg)
    }

    pub(crate) fn register(
        &mut self,
        def: TypeDef,
        name_id: StringId,
    ) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
        self.by_name.insert(name_id, id);
        self.defs.push(def);
        id
    }

    pub(crate) fn get_def(&self, id: TypeId) -> Option<&TypeDef> {
        self.defs.get(id.idx())
    }

    /// Look up a type by its name.
    pub(crate) fn lookup(&self, name: StringId) -> Option<TypeId> {
        self.by_name.get(&name).copied()
    }

    /// Get the name of a type for error messages.
    pub(crate) fn type_name<'a>(
        &self,
        id: TypeId,
        arena: &'a ValueArena,
    ) -> Option<&'a str> {
        self.get_def(id).and_then(|def| match def {
            TypeDef::Builtin(b) => Some(b.name()),
            TypeDef::Sum { name, .. } => arena.get_str(*name),
        })
    }

    /// Get the name of a variant for error messages.
    pub(crate) fn variant_name<'a>(
        &self,
        ty: TypeId,
        idx: u8,
        arena: &'a ValueArena,
    ) -> Option<&'a str> {
        self.get_def(ty).and_then(|def| match def {
            TypeDef::Builtin(_) => None,
            TypeDef::Sum { variants, .. } => variants
                .iter()
                .find(|v| v.idx == idx)
                .and_then(|v| arena.get_str(v.name)),
        })
    }

    /// Look up a variant by name within a sum type.
    pub(crate) fn lookup_variant(
        &self,
        ty: TypeId,
        name: StringId,
    ) -> Option<&VariantDef> {
        self.get_def(ty).and_then(|def| match def {
            TypeDef::Builtin(_) => None,
            TypeDef::Sum { variants, .. } => {
                variants.iter().find(|v| v.name == name)
            }
        })
    }

    /// Register all built-in types (called from `new`).
    ///
    /// Registers in order: Bool, Int, Float, String, Array, Object, Option, Result, Char.
    /// Option and Result are at indices 6 and 7 respectively; Char is at index 8.
    fn register_builtins(&mut self, arena: &mut ValueArena) -> Result<()> {
        // Primitives (indices 0-5)
        let bool_name = arena.intern("Bool");
        self.register(TypeDef::Builtin(BuiltinType::Bool), bool_name);

        let int_name = arena.intern("Int");
        self.register(TypeDef::Builtin(BuiltinType::Int), int_name);

        let float_name = arena.intern("Float");
        self.register(TypeDef::Builtin(BuiltinType::Float), float_name);

        let string_name = arena.intern("String");
        self.register(TypeDef::Builtin(BuiltinType::String), string_name);

        let array_name = arena.intern("Array");
        self.register(TypeDef::Builtin(BuiltinType::Array), array_name);

        let object_name = arena.intern("Object");
        self.register(TypeDef::Builtin(BuiltinType::Object), object_name);

        // Option[T] at index 6
        let option_name = arena.intern("Option");
        let t_param = arena.intern("T");
        let none_name = arena.intern("None");
        let some_name = arena.intern("Some");

        let opt = self.register(
            TypeDef::Sum {
                name: option_name,
                type_params: smallvec::smallvec![t_param],
                variants: smallvec::smallvec![
                    VariantDef {
                        name: none_name,
                        idx: 0,
                        arity: 0
                    },
                    VariantDef {
                        name: some_name,
                        idx: 1,
                        arity: 1
                    },
                ],
            },
            option_name,
        );
        (opt == TypeId::OPTION).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Option at index {}, expected {}",
                opt.0,
                TypeId::OPTION.0
            ))
        })?;

        // Result[T, E] at index 7
        let result_name = arena.intern("Result");
        let e_param = arena.intern("E");
        let ok_name = arena.intern("Ok");
        let err_name = arena.intern("Err");

        let res = self.register(
            TypeDef::Sum {
                name: result_name,
                type_params: smallvec::smallvec![t_param, e_param],
                variants: smallvec::smallvec![
                    VariantDef {
                        name: ok_name,
                        idx: 0,
                        arity: 1
                    },
                    VariantDef {
                        name: err_name,
                        idx: 1,
                        arity: 1
                    },
                ],
            },
            result_name,
        );
        (res == TypeId::RESULT).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Result at index {}, expected {}",
                res.0,
                TypeId::RESULT.0
            ))
        })?;

        // Char at index 8
        let char_name = arena.intern("Char");
        let ch = self.register(TypeDef::Builtin(BuiltinType::Char), char_name);
        (ch == TypeId::CHAR).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Char at index {}, expected {}",
                ch.0,
                TypeId::CHAR.0
            ))
        })?;

        Ok(())
    }

    fn len(&self) -> usize {
        self.defs.len()
    }

    fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: unwrap `Option.Some(v)` or `Result.Ok(v)`.
    fn unwrap_inner(v: &Value, type_exprs: &TypeExprArena) -> Option<ValueId> {
        match v {
            Value::Tagged(ty_expr, 1, p)
                if type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                p.first().copied()
            }
            Value::Tagged(ty_expr, 0, p)
                if type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                p.first().copied()
            }
            _ => None,
        }
    }

    #[test]
    fn string_interning() {
        let mut arena = ValueArena::new();

        let id1 = arena.intern("hello");
        let id2 = arena.intern("world");
        let id3 = arena.intern("hello");

        assert_eq!(id1, id3);
        assert_ne!(id1, id2);
        assert_eq!(arena.string_count(), 2);
        assert_eq!(arena.get_str(id1), Some("hello"));
        assert_eq!(arena.get_str(id2), Some("world"));
    }

    #[test]
    fn value_arena_basic() {
        let mut arena = ValueArena::new();

        let id1 = arena.add(Value::Int(42), Span::new(0, 2));
        let id2 = arena.add(Value::Bool(true), Span::new(3, 7));

        assert_eq!(arena.len(), 2);
        assert_eq!(arena.get(id1), Some(&Value::Int(42)));
        assert_eq!(arena.get(id2), Some(&Value::Bool(true)));
        assert_eq!(arena.span(id1), Some(Span::new(0, 2)));
    }

    #[test]
    fn builtin_types() {
        let mut arena = ValueArena::new();
        let reg = TypeRegistry::new(&mut arena).unwrap();

        assert_eq!(reg.len(), 9); // 7 primitives + Option + Result

        let bool_name = arena.intern("Bool");
        let option_name = arena.intern("Option");
        let result_name = arena.intern("Result");

        assert!(reg.lookup(bool_name).is_some());
        assert_eq!(reg.lookup(option_name), Some(TypeId::OPTION));
        assert_eq!(reg.lookup(result_name), Some(TypeId::RESULT));
    }

    #[test]
    fn option_values() {
        let mut arena = ValueArena::new();
        let _reg = TypeRegistry::new(&mut arena).unwrap();
        let mut type_exprs = TypeExprArena::new();

        // Option[Unknown] for None
        let unknown = type_exprs.named(TypeId::UNKNOWN);
        let opt_unknown =
            type_exprs.app(TypeId::OPTION, smallvec::smallvec![unknown]);

        let none = Value::none(opt_unknown);
        assert!(none.is_none(&type_exprs));
        assert!(!none.is_some(&type_exprs));

        // Option[Int] for Some(42)
        let int_ty = type_exprs.named(TypeId::INT);
        let opt_int =
            type_exprs.app(TypeId::OPTION, smallvec::smallvec![int_ty]);

        let inner = arena.add(Value::Int(42), Span::new(0, 2));
        let some = Value::some(opt_int, inner);
        assert!(!some.is_none(&type_exprs));
        assert!(some.is_some(&type_exprs));
        assert_eq!(unwrap_inner(&some, &type_exprs), Some(inner));
    }

    #[test]
    fn result_values() {
        let mut arena = ValueArena::new();
        let _reg = TypeRegistry::new(&mut arena).unwrap();
        let mut type_exprs = TypeExprArena::new();

        // Result[Int, Unknown] for Ok(42)
        let int_ty = type_exprs.named(TypeId::INT);
        let unknown = type_exprs.named(TypeId::UNKNOWN);
        let res_ok = type_exprs
            .app(TypeId::RESULT, smallvec::smallvec![int_ty, unknown]);

        let val = arena.add(Value::Int(42), Span::new(0, 2));
        let ok = Value::ok(res_ok, val);
        assert!(ok.is_ok(&type_exprs));
        assert!(!ok.is_err(&type_exprs));
        assert_eq!(unwrap_inner(&ok, &type_exprs), Some(val));

        // Result[Unknown, String] for Err("error")
        let str_ty = type_exprs.named(TypeId::STRING);
        let res_err = type_exprs
            .app(TypeId::RESULT, smallvec::smallvec![unknown, str_ty]);

        let err_str = arena.intern("error");
        let err_val = arena.add(Value::String(err_str), Span::new(3, 8));
        let err = Value::err(res_err, err_val);
        assert!(!err.is_ok(&type_exprs));
        assert!(err.is_err(&type_exprs));
        assert!(unwrap_inner(&err, &type_exprs).is_none()); // Err doesn't unwrap
    }

    #[test]
    fn truthy_falsy() {
        let mut arena = ValueArena::new();
        let _reg = TypeRegistry::new(&mut arena).unwrap();
        let mut type_exprs = TypeExprArena::new();
        let int_ty = type_exprs.named(TypeId::INT);
        let unknown = type_exprs.named(TypeId::UNKNOWN);

        // Falsy values
        assert!(!Value::Bool(false).is_truthy(&arena, &type_exprs));
        assert!(!Value::Int(0).is_truthy(&arena, &type_exprs));
        assert!(!Value::Float(OrderedFloat(0.0)).is_truthy(&arena, &type_exprs));

        let empty_str = arena.intern("");
        assert!(!Value::String(empty_str).is_truthy(&arena, &type_exprs));
        assert!(!Value::Array(int_ty, SmallVec::new())
            .is_truthy(&arena, &type_exprs));
        assert!(!Value::Object(IndexMap::new()).is_truthy(&arena, &type_exprs));

        let opt_unknown =
            type_exprs.app(TypeId::OPTION, smallvec::smallvec![unknown]);
        let none = Value::none(opt_unknown);
        assert!(!none.is_truthy(&arena, &type_exprs));

        let res_err_ty = type_exprs
            .app(TypeId::RESULT, smallvec::smallvec![unknown, int_ty]);
        let err_val = arena.add(Value::Int(42), Span::new(5, 7));
        let err = Value::err(res_err_ty, err_val);
        assert!(!err.is_truthy(&arena, &type_exprs));

        // Truthy values
        assert!(Value::Bool(true).is_truthy(&arena, &type_exprs));
        assert!(Value::Int(1).is_truthy(&arena, &type_exprs));
        assert!(Value::Float(OrderedFloat(0.1)).is_truthy(&arena, &type_exprs));

        let hello = arena.intern("hello");
        assert!(Value::String(hello).is_truthy(&arena, &type_exprs));

        let opt_int =
            type_exprs.app(TypeId::OPTION, smallvec::smallvec![int_ty]);
        let val = arena.add(Value::Int(1), Span::new(10, 11));
        let some = Value::some(opt_int, val);
        assert!(some.is_truthy(&arena, &type_exprs));

        let res_ok_ty = type_exprs
            .app(TypeId::RESULT, smallvec::smallvec![int_ty, unknown]);
        let ok = Value::ok(res_ok_ty, val);
        assert!(ok.is_truthy(&arena, &type_exprs));
    }

    #[test]
    fn variant_name_lookup() {
        let mut arena = ValueArena::new();
        let reg = TypeRegistry::new(&mut arena).unwrap();

        assert_eq!(reg.variant_name(TypeId::OPTION, 0, &arena), Some("None"));
        assert_eq!(reg.variant_name(TypeId::OPTION, 1, &arena), Some("Some"));

        assert_eq!(reg.variant_name(TypeId::RESULT, 0, &arena), Some("Ok"));
        assert_eq!(reg.variant_name(TypeId::RESULT, 1, &arena), Some("Err"));
    }
}
