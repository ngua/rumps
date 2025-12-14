//! Runtime value types with arena allocation and string interning.
//!
//! Uses arena allocation for cache efficiency and to avoid `Box` in recursive
//! structures. Strings are interned to avoid duplication and enable O(1)
//! comparison. A type registry enables runtime type validation, `is` checks,
//! and clear error messages.

#![allow(dead_code)]

use std::collections::HashMap;

use ordered_float::OrderedFloat;
use smallvec::SmallVec;

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
    const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Index into the type registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct TypeId(u32);

impl TypeId {
    /// Reserved index for the `Option` type.
    pub(crate) const OPTION: Self = Self(6);
    /// Reserved index for the `Result` type.
    pub(crate) const RESULT: Self = Self(7);

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
    strings: Vec<String>,
    string_map: HashMap<String, StringId>,
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
            strings: Vec::new(),
            string_map: HashMap::new(),
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
        self.string_map.get(s).copied().unwrap_or_else(|| {
            let id = StringId(self.strings.len() as u32);
            let owned = s.to_owned();
            self.string_map.insert(owned.clone(), id);
            self.strings.push(owned);
            id
        })
    }

    /// Get a string by its interned ID.
    pub(crate) fn get_str(&self, id: StringId) -> Option<&str> {
        self.strings.get(id.idx()).map(String::as_str)
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

    /// An interned string.
    String(StringId),

    /// An array of values.
    Array(Vec<ValueId>),

    /// An object/record with string keys.
    ///
    /// NOTE: `HashMap` does not preserve insertion order. If field ordering
    /// becomes important (e.g., for deterministic serialization), consider
    /// switching to `IndexMap` or `Vec<(StringId, ValueId)>`. The latter
    /// trades O(1) field access for ordering guarantees.
    Object(HashMap<StringId, ValueId>),

    /// A tagged value (sum type variant).
    ///
    /// - `TypeId`: the sum type (e.g., `Option`, `Result`)
    /// - `u8`: the variant index (e.g., `0` for `None`, `1` for `Some`)
    /// - `SmallVec`: the payload values (most variants have 0-2)
    Tagged(TypeId, u8, SmallVec<[ValueId; 2]>),
}

impl Value {
    /// Check if this value is truthy.
    ///
    /// Falsy values: `false`, `0`, `0.0`, `""`, `[]`, `{}`, `Option.None`, `Result.Err`
    pub(crate) fn is_truthy(&self, arena: &ValueArena) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Int(n) => *n != 0,
            Self::Float(f) => f.0 != 0.0,
            Self::String(id) => {
                arena.get_str(*id).map(|s| !s.is_empty()).unwrap_or(false)
            }
            Self::Array(arr) => !arr.is_empty(),
            Self::Object(obj) => !obj.is_empty(),
            Self::Tagged(ty, idx, _) => {
                // Option.None and Result.Err are falsy
                let is_none = *ty == TypeId::OPTION && *idx == 0;
                let is_err = *ty == TypeId::RESULT && *idx == 1;
                !(is_none || is_err)
            }
        }
    }

    /// Get the type name of this value for error messages.
    pub(crate) fn type_name(&self, reg: &TypeRegistry) -> &'static str {
        match self {
            Self::Bool(_) => "Bool",
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::String(_) => "String",
            Self::Array(_) => "Array",
            Self::Object(_) => "Object",
            Self::Tagged(ty, _, _) => reg
                .get_def(*ty)
                .map(|def| match def {
                    TypeDef::Builtin(b) => b.name(),
                    TypeDef::Sum { .. } => "Tagged",
                })
                .unwrap_or("Unknown"),
        }
    }

    /// Create an `Option.None` value.
    pub(crate) fn none() -> Self {
        Self::Tagged(TypeId::OPTION, 0, SmallVec::new())
    }

    /// Create an `Option.Some(v)` value.
    pub(crate) fn some(v: ValueId) -> Self {
        Self::Tagged(TypeId::OPTION, 1, smallvec::smallvec![v])
    }

    /// Create a `Result.Ok(v)` value.
    pub(crate) fn ok(v: ValueId) -> Self {
        Self::Tagged(TypeId::RESULT, 0, smallvec::smallvec![v])
    }

    /// Create a `Result.Err(e)` value.
    pub(crate) fn err(e: ValueId) -> Self {
        Self::Tagged(TypeId::RESULT, 1, smallvec::smallvec![e])
    }

    /// Check if this is `Option.None`.
    pub(crate) fn is_none(&self) -> bool {
        matches!(self, Self::Tagged(ty, 0, _) if *ty == TypeId::OPTION)
    }

    /// Check if this is `Option.Some`.
    pub(crate) fn is_some(&self) -> bool {
        matches!(self, Self::Tagged(ty, 1, _) if *ty == TypeId::OPTION)
    }

    /// Check if this is `Result.Ok`.
    pub(crate) fn is_ok(&self) -> bool {
        matches!(self, Self::Tagged(ty, 0, _) if *ty == TypeId::RESULT)
    }

    /// Check if this is `Result.Err`.
    pub(crate) fn is_err(&self) -> bool {
        matches!(self, Self::Tagged(ty, 1, _) if *ty == TypeId::RESULT)
    }
}

/// Built-in primitive types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinType {
    Bool,
    Int,
    Float,
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
/// Examples: `Int`, `Array[String]`, `Result[Int, String]`
#[derive(Clone, Debug, PartialEq, Eq)]
enum TypeExpr {
    Named(TypeId),
    App(TypeId, SmallVec<[TypeExprId; 2]>),
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

    /// Register all built-in types (called from `new`).
    ///
    /// Registers in order: Bool, Int, Float, String, Array, Object, Option, Result.
    /// Option and Result are at indices 6 and 7 respectively.
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

        Ok(())
    }

    fn len(&self) -> usize {
        self.defs.len()
    }

    fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Context needed for coercion operations.
struct CoerceCtx<'a> {
    arena: &'a ValueArena,
    registry: &'a TypeRegistry,
}

/// Trait for coercing a `Value` to a target type.
///
/// RUMPS uses conservative coercion:
/// - Values can be coerced INTO strings, but not OUT of them
/// - Numeric types can be coerced between each other
trait Coerce<T> {
    fn coerce(&self, ctx: &CoerceCtx<'_>) -> Result<T>;
}

impl Coerce<String> for Value {
    fn coerce(&self, ctx: &CoerceCtx<'_>) -> Result<String> {
        fn stringify(
            v: &Value,
            arena: &ValueArena,
            reg: &TypeRegistry,
        ) -> String {
            match v {
                Value::Bool(b) => b.to_string(),
                Value::Int(n) => n.to_string(),
                Value::Float(f) => f.to_string(),
                Value::String(id) => {
                    arena.get_str(*id).unwrap_or("").to_owned()
                }
                Value::Array(arr) => {
                    let items = arr
                        .iter()
                        .filter_map(|id| arena.get(*id))
                        .map(|v| stringify(v, arena, reg))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("[ {items} ]")
                }
                Value::Object(obj) => {
                    let fields = obj
                        .iter()
                        .map(|(k, v)| {
                            let key = arena.get_str(*k).unwrap_or("?");
                            let val = arena
                                .get(*v)
                                .map(|v| stringify(v, arena, reg))
                                .unwrap_or_else(|| "?".to_owned());
                            format!("{key}: {val}")
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{{ {fields} }}")
                }
                Value::Tagged(ty, idx, payloads) => {
                    let ty_name = reg.type_name(*ty, arena).unwrap_or("?");
                    let var_name =
                        reg.variant_name(*ty, *idx, arena).unwrap_or("?");

                    if payloads.is_empty() {
                        format!("{ty_name}.{var_name}")
                    } else {
                        let args = payloads
                            .iter()
                            .filter_map(|id| arena.get(*id))
                            .map(|v| stringify(v, arena, reg))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{ty_name}.{var_name}({args})")
                    }
                }
            }
        }

        Ok(stringify(self, ctx.arena, ctx.registry))
    }
}

impl Coerce<i64> for Value {
    fn coerce(&self, ctx: &CoerceCtx<'_>) -> Result<i64> {
        match self {
            Self::Int(n) => Ok(*n),
            Self::Float(f) => Ok(f.0 as i64),
            Self::Bool(b) => Ok(if *b { 1 } else { 0 }),
            _ => Err(coercion_err(self.type_name(ctx.registry), "Int")),
        }
    }
}

impl Coerce<OrderedFloat<f64>> for Value {
    fn coerce(&self, ctx: &CoerceCtx<'_>) -> Result<OrderedFloat<f64>> {
        match self {
            Self::Int(n) => Ok(OrderedFloat(*n as f64)),
            Self::Float(f) => Ok(*f),
            Self::Bool(b) => Ok(OrderedFloat(if *b { 1.0 } else { 0.0 })),
            _ => Err(coercion_err(self.type_name(ctx.registry), "Float")),
        }
    }
}

impl Coerce<bool> for Value {
    fn coerce(&self, ctx: &CoerceCtx<'_>) -> Result<bool> {
        Ok(self.is_truthy(ctx.arena))
    }
}

/// Create a coercion error.
fn coercion_err(from: &'static str, to: &'static str) -> crate::Error {
    crate::Error::coercion(from, to, format!("cannot coerce {from} to {to}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: unwrap `Option.Some(v)` or `Result.Ok(v)`.
    fn unwrap_inner(v: &Value) -> Option<ValueId> {
        match v {
            Value::Tagged(ty, 1, p) if *ty == TypeId::OPTION => {
                p.first().copied()
            }
            Value::Tagged(ty, 0, p) if *ty == TypeId::RESULT => {
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

        assert_eq!(reg.len(), 8); // 6 primitives + Option + Result

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

        let none = Value::none();
        assert!(none.is_none());
        assert!(!none.is_some());

        let inner = arena.add(Value::Int(42), Span::new(0, 2));
        let some = Value::some(inner);
        assert!(!some.is_none());
        assert!(some.is_some());
        assert_eq!(unwrap_inner(&some), Some(inner));
    }

    #[test]
    fn result_values() {
        let mut arena = ValueArena::new();
        let _reg = TypeRegistry::new(&mut arena).unwrap();

        let val = arena.add(Value::Int(42), Span::new(0, 2));
        let ok = Value::ok(val);
        assert!(ok.is_ok());
        assert!(!ok.is_err());
        assert_eq!(unwrap_inner(&ok), Some(val));

        let err_str = arena.intern("error");
        let err_val = arena.add(Value::String(err_str), Span::new(3, 8));
        let err = Value::err(err_val);
        assert!(!err.is_ok());
        assert!(err.is_err());
        assert!(unwrap_inner(&err).is_none()); // Err doesn't unwrap
    }

    #[test]
    fn truthy_falsy() {
        let mut arena = ValueArena::new();
        let _reg = TypeRegistry::new(&mut arena).unwrap();

        // Falsy values
        assert!(!Value::Bool(false).is_truthy(&arena));
        assert!(!Value::Int(0).is_truthy(&arena));
        assert!(!Value::Float(OrderedFloat(0.0)).is_truthy(&arena));

        let empty_str = arena.intern("");
        assert!(!Value::String(empty_str).is_truthy(&arena));
        assert!(!Value::Array(vec![]).is_truthy(&arena));
        assert!(!Value::Object(HashMap::new()).is_truthy(&arena));

        let none = Value::none();
        assert!(!none.is_truthy(&arena));

        let err_val = arena.add(Value::Int(42), Span::new(5, 7));
        let err = Value::err(err_val);
        assert!(!err.is_truthy(&arena));

        // Truthy values
        assert!(Value::Bool(true).is_truthy(&arena));
        assert!(Value::Int(1).is_truthy(&arena));
        assert!(Value::Float(OrderedFloat(0.1)).is_truthy(&arena));

        let hello = arena.intern("hello");
        assert!(Value::String(hello).is_truthy(&arena));

        let val = arena.add(Value::Int(1), Span::new(10, 11));
        let some = Value::some(val);
        assert!(some.is_truthy(&arena));

        let ok = Value::ok(val);
        assert!(ok.is_truthy(&arena));
    }

    #[test]
    fn coercion_to_string() {
        let mut arena = ValueArena::new();
        let reg = TypeRegistry::new(&mut arena).unwrap();
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };

        assert_eq!(
            <Value as Coerce<String>>::coerce(&Value::Int(42), &ctx).unwrap(),
            "42"
        );
        assert_eq!(
            <Value as Coerce<String>>::coerce(&Value::Bool(true), &ctx)
                .unwrap(),
            "true"
        );
        assert_eq!(
            <Value as Coerce<String>>::coerce(
                &Value::Float(OrderedFloat(3.14)),
                &ctx
            )
            .unwrap(),
            "3.14"
        );

        // Test array
        let v1 = arena.add(Value::Int(1), Span::new(0, 1));
        let v2 = arena.add(Value::Int(2), Span::new(3, 4));
        let arr = Value::Array(vec![v1, v2]);
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };
        assert_eq!(
            <Value as Coerce<String>>::coerce(&arr, &ctx).unwrap(),
            "[ 1, 2 ]"
        );

        // Test object
        let k = arena.intern("x");
        let v = arena.add(Value::Int(10), Span::new(6, 8));
        let obj = Value::Object(std::iter::once((k, v)).collect());
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };
        assert_eq!(
            <Value as Coerce<String>>::coerce(&obj, &ctx).unwrap(),
            "{ x: 10 }"
        );

        // Test tagged (Option.Some)
        let inner = arena.add(Value::Int(42), Span::new(10, 12));
        let some = Value::some(inner);
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };
        assert_eq!(
            <Value as Coerce<String>>::coerce(&some, &ctx).unwrap(),
            "Option.Some(42)"
        );

        // Test tagged (Option.None)
        let none = Value::none();
        assert_eq!(
            <Value as Coerce<String>>::coerce(&none, &ctx).unwrap(),
            "Option.None"
        );
    }

    #[test]
    fn coercion_numeric() {
        let mut arena = ValueArena::new();
        let reg = TypeRegistry::new(&mut arena).unwrap();
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };

        assert_eq!(
            <Value as Coerce<i64>>::coerce(
                &Value::Float(OrderedFloat(3.7)),
                &ctx
            )
            .unwrap(),
            3
        );
        assert_eq!(
            <Value as Coerce<OrderedFloat<f64>>>::coerce(&Value::Int(42), &ctx)
                .unwrap(),
            OrderedFloat(42.0)
        );

        // String to Int should fail
        let s = arena.intern("42");
        let ctx = CoerceCtx {
            arena: &arena,
            registry: &reg,
        };
        assert!(
            <Value as Coerce<i64>>::coerce(&Value::String(s), &ctx).is_err()
        );
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
