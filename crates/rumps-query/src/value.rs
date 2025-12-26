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

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use crate::ast::{AstTypeExprId, ExprId};
use crate::intern::{StringId, StringInterner};
use crate::typecheck::Ty;
use crate::{Result, Span};

/// A hashable key for `Map` values.
///
/// Map keys are restricted to scalar types for hashability. This enum wraps
/// scalar values with proper `Hash` and `Eq` implementations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MapKey {
    Bool(bool),
    Int(i64),
    Float(OrderedFloat<f64>),
    Char(char),
    String(StringId),
}

impl MapKey {
    /// Convert a `Value` to a `MapKey`, or `None` if not a scalar type.
    pub(crate) fn from_value(v: &Value) -> Option<Self> {
        match v {
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Int(n) => Some(Self::Int(*n)),
            Value::Float(f) => Some(Self::Float(*f)),
            Value::Char(c) => Some(Self::Char(*c)),
            Value::String(sid) => Some(Self::String(*sid)),
            _ => None,
        }
    }

    /// Convert a `MapKey` back to a `Value`.
    pub(crate) fn to_value(&self) -> Value {
        match self {
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(n) => Value::Int(*n),
            Self::Float(f) => Value::Float(*f),
            Self::Char(c) => Value::Char(*c),
            Self::String(sid) => Value::String(*sid),
        }
    }

    /// Get the `TypeId` for this key.
    pub(crate) fn type_id(&self) -> TypeId {
        match self {
            Self::Bool(_) => TypeId::BOOL,
            Self::Int(_) => TypeId::INT,
            Self::Float(_) => TypeId::FLOAT,
            Self::Char(_) => TypeId::CHAR,
            Self::String(_) => TypeId::STRING,
        }
    }
}

/// Index into the value arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct ValueId(u32);

impl ValueId {
    const fn idx(self) -> usize {
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
    /// Builtin type: `Tuple`.
    pub(crate) const TUPLE: Self = Self(9);
    /// Builtin type: `Map`.
    pub(crate) const MAP: Self = Self(10);
    /// Builtin type: `Time`.
    pub(crate) const TIME: Self = Self(11);
    /// Builtin type: `Range`.
    pub(crate) const RANGE: Self = Self(12);
    /// Builtin type: `Unit`.
    pub(crate) const UNIT: Self = Self(13);
    /// Builtin type: `Json`.
    pub(crate) const JSON: Self = Self(14);
    /// Builtin union: `Storable = Bool | Int | Float | Char | String | Json`.
    ///
    /// The set of types that can be stored in B-tree globals/locals.
    /// `AS Storable` is infallible; `AS` to other unions requires `READ`.
    pub(crate) const STORABLE: Self = Self(15);
    /// Builtin union: `Scalar = Bool | Int | Float | String`.
    ///
    /// Used as return type for `->>`  JSON scalar extraction.
    pub(crate) const SCALAR: Self = Self(16);
    /// Placeholder type for uninferred type parameters; compatible with any type.
    /// Used for empty arrays (unknown element type) and partial variant types
    /// (e.g., `Option.None` has unknown `T`, `Result.Ok(v)` has unknown `E`).
    /// Note: This is NOT a registered type; it's a marker used in type expressions.
    pub(crate) const UNKNOWN: Self = Self(u32::MAX);

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
    /// Shared with `TypeEnv` so type lookups use consistent `StringId`s.
    pub(crate) strings: StringInterner,
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
            strings: StringInterner::new(),
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

    /// Get the base type of a value by ID without cloning.
    pub(crate) fn base_type_of(
        &self,
        id: ValueId,
        type_exprs: &TypeExprArena,
    ) -> Option<TypeId> {
        self.get(id).map(|v| v.base_type(type_exprs))
    }

    /// Get the span of a value.
    pub(crate) fn span(&self, id: ValueId) -> Option<Span> {
        self.value_spans.get(id.idx()).copied()
    }

    /// Intern a string, returning its ID.
    ///
    /// If the string is already interned, returns the existing ID.
    pub(crate) fn intern(&mut self, s: &str) -> StringId {
        self.strings.intern(s)
    }

    /// Get a string by its interned ID.
    pub(crate) fn get_str(&self, id: StringId) -> Option<&str> {
        self.strings.get(id)
    }

    /// Look up a string's ID without interning it.
    ///
    /// Returns `None` if the string has not been interned.
    pub(crate) fn lookup_string(&self, s: &str) -> Option<StringId> {
        self.strings.lookup(s)
    }

    /// Get array elements by ID, cloning only the element vector.
    ///
    /// Returns `None` if the value doesn't exist or isn't an array.
    pub(crate) fn get_array(
        &self,
        id: ValueId,
    ) -> Option<(TypeExprId, SmallVec<[ValueId; 4]>)> {
        match self.get(id)? {
            Value::Array(ty, elems) => Some((*ty, elems.clone())),
            _ => None,
        }
    }

    /// Get object fields by ID, cloning only the field map.
    ///
    /// Returns `None` if the value doesn't exist or isn't an object.
    pub(crate) fn get_object(
        &self,
        id: ValueId,
    ) -> Option<IndexMap<StringId, ValueId>> {
        match self.get(id)? {
            Value::Object(map) => Some(map.clone()),
            _ => None,
        }
    }

    /// Get tuple elements by ID, cloning only the element vector.
    ///
    /// Returns `None` if the value doesn't exist or isn't a tuple.
    pub(crate) fn get_tuple(
        &self,
        id: ValueId,
    ) -> Option<(TypeExprId, SmallVec<[ValueId; 4]>)> {
        match self.get(id)? {
            Value::Tuple(ty, elems) => Some((*ty, elems.clone())),
            _ => None,
        }
    }

    /// Get string ID from a value, returning the interned `StringId`.
    ///
    /// Returns `None` if the value doesn't exist or isn't a string.
    pub(crate) fn get_string_id(&self, id: ValueId) -> Option<StringId> {
        match self.get(id)? {
            Value::String(sid) => Some(*sid),
            _ => None,
        }
    }

    /// Get a reference to map contents by ID (no cloning).
    ///
    /// Returns `None` if the value doesn't exist or isn't a map.
    pub(crate) fn get_map_ref(
        &self,
        id: ValueId,
    ) -> Option<(TypeExprId, TypeExprId, &IndexMap<MapKey, ValueId>)> {
        match self.get(id)? {
            Value::Map(k_ty, v_ty, entries) => Some((*k_ty, *v_ty, entries)),
            _ => None,
        }
    }

    /// Get map contents by ID, cloning the key-value map.
    ///
    /// Use `get_map_ref` for read-only access to avoid cloning.
    ///
    /// Returns `None` if the value doesn't exist or isn't a map.
    pub(crate) fn get_map(
        &self,
        id: ValueId,
    ) -> Option<(TypeExprId, TypeExprId, IndexMap<MapKey, ValueId>)> {
        match self.get(id)? {
            Value::Map(k_ty, v_ty, entries) => {
                Some((*k_ty, *v_ty, entries.clone()))
            }
            _ => None,
        }
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

    /// Clone the string interner for type checking.
    ///
    /// The type checker needs a separate copy of the interner because
    /// `TypeEnv` is consumed at the end of type checking. Interned strings
    /// are shared by reference (both copies have the same `StringId` mappings).
    pub(crate) fn interner(&self) -> StringInterner {
        self.strings.clone()
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

    /// Get the bindings map (for restoring scope from captured environment).
    pub(crate) fn bindings(&self) -> &HashMap<StringId, ValueId> {
        &self.bindings
    }
}

/// A runtime value.
///
/// Uses `StringId` for interned strings and `ValueId` for nested values,
/// avoiding allocation and enabling O(1) string comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    /// The unit value; represents "no meaningful value".
    ///
    /// Used for statements, blocks without trailing expressions, and
    /// single-arm `IF` (side-effect only).
    Unit,

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

    /// A tuple value (heterogeneous, fixed-size sequence).
    ///
    /// Unlike arrays, tuples can hold different types and support positional
    /// access (`.0`, `.1`, etc.). The `TypeExprId` encodes the element types.
    Tuple(TypeExprId, SmallVec<[ValueId; 4]>),

    /// A homogeneous map with typed keys and values.
    ///
    /// - First `TypeExprId`: key type (K)
    /// - Second `TypeExprId`: value type (V)
    /// - `IndexMap<MapKey, ValueId>`: key-value pairs (insertion order preserved)
    ///
    /// Keys are restricted to scalar types (Bool, Int, Float, Char, String).
    Map(TypeExprId, TypeExprId, IndexMap<MapKey, ValueId>),

    /// A point in time (UTC).
    Time(DateTime<Utc>),

    /// An opaque JSON value.
    ///
    /// Wraps `serde_json::Value`. JSON values are created from:
    /// - Object literals with quoted keys: `{ "id": 123 }`
    /// - Heterogeneous array literals: `[1, "two", true]`
    /// - Explicit cast: `value AS Json`
    ///
    /// Access via `.` and `->` returns `Json`; `..` and `->>` extract scalars.
    Json(serde_json::Value),

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

    /// A named function reference.
    ///
    /// When a named function is referenced without being called (e.g., `f` instead
    /// of `f(x)`), it produces this value. This enables passing functions to
    /// higher-order functions.
    Function {
        name: StringId,
        params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
        ret: Option<TypeExprId>,
        body: ExprId,
    },

    /// A module function reference.
    ///
    /// Created when a module path like `Array.length` is evaluated. Can be
    /// called directly or used as a first-class value (e.g., in pipelines).
    ///
    /// The path includes the full module path plus function name:
    /// - `Array.length` → `["Array", "length"]`
    /// - `Math.Trig.sin` → `["Math", "Trig", "sin"]`
    ModuleFn { path: SmallVec<[StringId; 4]> },

    /// A lazy integer range.
    ///
    /// Created by `start..end` (exclusive) or `start..=end` (inclusive).
    /// Does not allocate; used with collection operations like `Array.map`.
    ///
    /// - `start`: the first value in the range
    /// - `end`: the bound (exclusive or inclusive depending on `inclusive`)
    /// - `inclusive`: `true` for `..=`, `false` for `..`
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
}

impl Value {
    /// Get the type name of this value for error messages.
    ///
    /// Returns a structural type representation for objects (e.g., `{ name: String }`).
    /// Other types return their simple names.
    pub(crate) fn type_name(
        &self,
        reg: &TypeRegistry,
        type_exprs: &TypeExprArena,
        arena: &ValueArena,
    ) -> Cow<'static, str> {
        match self {
            Self::Unit => Cow::Borrowed("Unit"),
            Self::Bool(_) => Cow::Borrowed("Bool"),
            Self::Int(_) => Cow::Borrowed("Int"),
            Self::Float(_) => Cow::Borrowed("Float"),
            Self::Char(_) => Cow::Borrowed("Char"),
            Self::String(_) => Cow::Borrowed("String"),
            Self::Array(..) => Cow::Borrowed("Array"),
            Self::Object(fields) => {
                // Build structural type: `{ field: Type, ... }`
                let parts: Vec<_> = fields
                    .iter()
                    .filter_map(|(name_id, val_id)| {
                        let name = arena.get_str(*name_id)?;
                        let val = arena.get(*val_id)?;
                        let ty = val.type_name(reg, type_exprs, arena);
                        Some(format!("{name}: {ty}"))
                    })
                    .collect();
                Cow::Owned(format!("{{ {} }}", parts.join(", ")))
            }
            Self::Tuple(..) => Cow::Borrowed("Tuple"),
            Self::Map(..) => Cow::Borrowed("Map"),
            Self::Time(_) => Cow::Borrowed("Time"),
            Self::Json(_) => Cow::Borrowed("Json"),
            Self::Tagged(ty_expr, _, _) => Cow::Borrowed(
                type_exprs
                    .base_type(*ty_expr)
                    .and_then(|ty| reg.get_def(ty))
                    .map(|def| match def {
                        TypeDef::Builtin(b) => b.name(),
                        TypeDef::Sum { .. } => "Tagged",
                        TypeDef::Struct { .. } => "Struct",
                        TypeDef::Union { .. } => "Union",
                    })
                    .unwrap_or("Unknown"),
            ),
            Self::Closure { .. } => Cow::Borrowed("Closure"),
            Self::Function { .. } => Cow::Borrowed("Function"),
            Self::ModuleFn { .. } => Cow::Borrowed("ModuleFn"),
            Self::Range { .. } => Cow::Borrowed("Range"),
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

    /// Check if this is `Option.Some(_)`.
    pub(crate) fn is_some(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 1, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::OPTION),
            _ => false,
        }
    }

    /// Check if this is `Result.Ok(_)`.
    pub(crate) fn is_ok(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 0, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::RESULT),
            _ => false,
        }
    }

    /// Check if this is `Result.Err(_)`.
    pub(crate) fn is_err(&self, type_exprs: &TypeExprArena) -> bool {
        match self {
            Self::Tagged(ty_expr, 1, _) => type_exprs
                .base_type(*ty_expr)
                .is_some_and(|ty| ty == TypeId::RESULT),
            _ => false,
        }
    }

    /// Get the simplified base `TypeId` for this value.
    ///
    /// Returns the primitive type id for scalars. For tagged values, resolves
    /// the base type from the type expression (e.g., `Option` or `Result`).
    /// Returns `UNKNOWN` for closures and functions.
    pub(crate) fn base_type(&self, type_exprs: &TypeExprArena) -> TypeId {
        match self {
            Self::Unit => TypeId::UNIT,
            Self::Bool(_) => TypeId::BOOL,
            Self::Int(_) => TypeId::INT,
            Self::Float(_) => TypeId::FLOAT,
            Self::Char(_) => TypeId::CHAR,
            Self::String(_) => TypeId::STRING,
            Self::Array(..) => TypeId::ARRAY,
            Self::Object(_) => TypeId::OBJECT,
            Self::Tuple(..) => TypeId::TUPLE,
            Self::Map(..) => TypeId::MAP,
            Self::Time(_) => TypeId::TIME,
            Self::Json(_) => TypeId::JSON,
            Self::Tagged(ty_expr, _, _) => {
                type_exprs.base_type(*ty_expr).unwrap_or(TypeId::UNKNOWN)
            }
            Self::Closure { .. }
            | Self::Function { .. }
            | Self::ModuleFn { .. } => TypeId::UNKNOWN,
            Self::Range { .. } => TypeId::RANGE,
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
    Tuple,
    Map,
    Time,
    Range,
    Unit,
    Json,
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
            Self::Tuple => "Tuple",
            Self::Map => "Map",
            Self::Time => "Time",
            Self::Range => "Range",
            Self::Unit => "Unit",
            Self::Json => "Json",
        }
    }
}

/// A variant definition for sum types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VariantDef {
    pub(crate) name: StringId,
    pub(crate) idx: u8,
    pub(crate) arity: u8,
    /// Payload types for this variant (AST type expression IDs).
    /// Used by type checker to get payload types for pattern matching.
    pub(crate) payloads: SmallVec<[AstTypeExprId; 2]>,
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
    /// Structural object type alias.
    ///
    /// Maps field names to their expected types. At runtime, values are
    /// `Value::Object`; the struct type is used for optional validation
    /// when assigning to a typed variable (`LET x: StructName = ...`).
    Struct {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        /// Field types stored as AST expressions (not resolved) to support
        /// type parameters. Resolution happens at usage site with substitution.
        fields: IndexMap<StringId, AstTypeExprId>,
    },
    /// Named union type definition.
    ///
    /// Union types represent a value that can be one of several types.
    /// Used for `UNION Storable = Bool | Int | ...` declarations.
    /// At runtime, `IS` checks test against each member; `AS` casts are
    /// infallible only for `Storable` (special-cased).
    Union {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        /// Member type expressions (stored in `TypeExprArena`).
        members: SmallVec<[TypeExprId; 8]>,
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
    /// Tuple type: `(Int, String, Bool)`
    Tuple(SmallVec<[TypeExprId; 4]>),
    /// Union type: `Int | String | Bool`
    ///
    /// A value matches a union if it matches ANY member type.
    Union(SmallVec<[TypeExprId; 4]>),
    /// Structural object type: `{ field: Type, ... }`
    ///
    /// Anonymous structural object type. A value matches if it has at least
    /// the specified fields with matching types (extensible record semantics).
    Object(IndexMap<StringId, TypeExprId>),
}

/// A named function definition stored in the function registry.
///
/// Created from `Stmt::Fun` during interpretation; the names and type
/// annotations are resolved to interned IDs.
#[derive(Clone, Debug)]
pub(crate) struct FunctionDef {
    pub(crate) name: StringId,
    pub(crate) params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
    pub(crate) ret: Option<TypeExprId>,
    pub(crate) body: ExprId,
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
    /// For `Fn`, `Tuple`, `Union`, and `Object` returns `None` (compound types
    /// have no single base).
    pub(crate) fn base_type(&self, id: TypeExprId) -> Option<TypeId> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Named(ty) | TypeExpr::App(ty, _) => Some(*ty),
            TypeExpr::Fn(..)
            | TypeExpr::Tuple(..)
            | TypeExpr::Union(..)
            | TypeExpr::Object(..) => None,
        })
    }

    /// Get type arguments from a parameterized type expression.
    ///
    /// For `App(T, params)` returns `Some(&params)`; for `Named(T)` returns `None`.
    pub(crate) fn type_args(
        &self,
        id: TypeExprId,
    ) -> Option<&SmallVec<[TypeExprId; 2]>> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::App(_, params) => Some(params),
            _ => None,
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

    /// Get function type parts: `(params, return_type)`.
    ///
    /// Returns `None` if the type expression is not a function type.
    pub(crate) fn fn_parts(
        &self,
        id: TypeExprId,
    ) -> Option<(&SmallVec<[TypeExprId; 4]>, TypeExprId)> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Fn(params, ret) => Some((params, *ret)),
            _ => None,
        })
    }

    /// Check if a type expression is a function type.
    pub(crate) fn is_fn(&self, id: TypeExprId) -> bool {
        self.get(id).is_some_and(|e| matches!(e, TypeExpr::Fn(..)))
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
            (TypeExpr::Tuple(ea), TypeExpr::Tuple(eb)) => {
                ea.len() == eb.len()
                    && ea.iter().zip(eb.iter()).all(|(a, b)| self.eq(*a, *b))
            }
            (TypeExpr::Union(ma), TypeExpr::Union(mb)) => {
                ma.len() == mb.len()
                    && ma.iter().zip(mb.iter()).all(|(a, b)| self.eq(*a, *b))
            }
            (TypeExpr::Object(fa), TypeExpr::Object(fb)) => {
                fa.len() == fb.len()
                    && fa.iter().all(|(k, va)| {
                        fb.get(k).is_some_and(|vb| self.eq(*va, *vb))
                    })
            }
            _ => false,
        }
    }

    /// Format a type expression for display.
    ///
    /// - `name_fn`: converts `TypeId` to a type name string
    /// - `str_fn`: converts `StringId` to a string (for object field names)
    pub(crate) fn format<F, S>(
        &self,
        id: TypeExprId,
        name_fn: F,
        str_fn: S,
    ) -> Option<String>
    where
        F: Fn(TypeId) -> String + Copy,
        S: Fn(StringId) -> String + Copy,
    {
        self.get(id)
            .map(|expr| self.format_expr(expr, name_fn, str_fn))
    }

    fn format_expr<F, S>(
        &self,
        expr: &TypeExpr,
        name_fn: F,
        str_fn: S,
    ) -> String
    where
        F: Fn(TypeId) -> String + Copy,
        S: Fn(StringId) -> String + Copy,
    {
        match expr {
            TypeExpr::Named(ty) => name_fn(*ty),
            TypeExpr::App(ty, params) => {
                let name = name_fn(*ty);
                let args = params
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn, str_fn))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{name}[{args}]")
            }
            TypeExpr::Fn(params, ret) => {
                let args = params
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn, str_fn))
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_str = self
                    .format(*ret, name_fn, str_fn)
                    .unwrap_or_else(|| "?".to_owned());
                format!("({args}) -> {ret_str}")
            }
            TypeExpr::Tuple(elems) => {
                let parts = elems
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn, str_fn))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({parts})")
            }
            TypeExpr::Union(members) => {
                let parts = members
                    .iter()
                    .filter_map(|p| self.format(*p, name_fn, str_fn))
                    .collect::<Vec<_>>()
                    .join(" | ");
                parts
            }
            TypeExpr::Object(fields) => {
                let parts = fields
                    .iter()
                    .map(|(k, v)| {
                        let name = str_fn(*k);
                        let ty = self
                            .format(*v, name_fn, str_fn)
                            .unwrap_or_else(|| "?".to_owned());
                        format!("{name}: {ty}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{ {parts} }}")
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

    /// Add a tuple type expression (e.g., `(Int, String, Bool)`).
    pub(crate) fn tuple(
        &mut self,
        elems: SmallVec<[TypeExprId; 4]>,
    ) -> TypeExprId {
        self.add(TypeExpr::Tuple(elems))
    }

    /// Get tuple element types if this is a tuple type.
    pub(crate) fn tuple_elems(
        &self,
        id: TypeExprId,
    ) -> Option<&SmallVec<[TypeExprId; 4]>> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Tuple(elems) => Some(elems),
            _ => None,
        })
    }

    /// Check if a type expression is a tuple type.
    pub(crate) fn is_tuple(&self, id: TypeExprId) -> bool {
        self.get(id)
            .is_some_and(|e| matches!(e, TypeExpr::Tuple(..)))
    }

    /// Add a union type expression (e.g., `Int | String | Bool`).
    pub(crate) fn union(
        &mut self,
        members: SmallVec<[TypeExprId; 4]>,
    ) -> TypeExprId {
        self.add(TypeExpr::Union(members))
    }

    /// Get union member types if this is a union type.
    pub(crate) fn union_members(
        &self,
        id: TypeExprId,
    ) -> Option<&SmallVec<[TypeExprId; 4]>> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Union(members) => Some(members),
            _ => None,
        })
    }

    /// Check if a type expression is a union type.
    pub(crate) fn is_union(&self, id: TypeExprId) -> bool {
        self.get(id)
            .is_some_and(|e| matches!(e, TypeExpr::Union(..)))
    }

    /// Add a structural object type expression (e.g., `{ name: String, age: Int }`).
    pub(crate) fn object(
        &mut self,
        fields: IndexMap<StringId, TypeExprId>,
    ) -> TypeExprId {
        self.add(TypeExpr::Object(fields))
    }

    /// Get object field types if this is a structural object type.
    pub(crate) fn object_fields(
        &self,
        id: TypeExprId,
    ) -> Option<&IndexMap<StringId, TypeExprId>> {
        self.get(id).and_then(|expr| match expr {
            TypeExpr::Object(fields) => Some(fields),
            _ => None,
        })
    }

    /// Check if a type expression is a structural object type.
    pub(crate) fn is_object(&self, id: TypeExprId) -> bool {
        self.get(id)
            .is_some_and(|e| matches!(e, TypeExpr::Object(..)))
    }

    /// Convert a resolved static type to a runtime type expression.
    ///
    /// Used after type checking to create runtime type tags for:
    /// - Runtime `IS` checks (compare value's type tag against annotation)
    /// - Runtime `AS` casts (verify cast is valid)
    /// - Error messages with concrete types
    ///
    /// # Panics
    ///
    /// Panics if `ty` contains unresolved type variables (`Var`, `Unknown`, `Error`).
    /// These should be resolved during constraint solving before calling this.
    #[allow(dead_code)]
    pub(crate) fn from_ty(&mut self, ty: &Ty) -> TypeExprId {
        match ty {
            Ty::Bool => self.named(TypeId::BOOL),
            Ty::Int => self.named(TypeId::INT),
            Ty::Float => self.named(TypeId::FLOAT),
            Ty::Char => self.named(TypeId::CHAR),
            Ty::String => self.named(TypeId::STRING),
            Ty::Unit => self.named(TypeId::UNIT),
            Ty::Time => self.named(TypeId::TIME),
            Ty::Range => self.named(TypeId::RANGE),
            Ty::Json => self.named(TypeId::JSON),
            Ty::Array(elem) => {
                let elem_id = self.from_ty(elem);
                self.app(TypeId::ARRAY, smallvec![elem_id])
            }
            Ty::Option(inner) => {
                let inner_id = self.from_ty(inner);
                self.app(TypeId::OPTION, smallvec![inner_id])
            }
            Ty::Result(ok, err) => {
                let ok_id = self.from_ty(ok);
                let err_id = self.from_ty(err);
                self.app(TypeId::RESULT, smallvec![ok_id, err_id])
            }
            Ty::Map(k, v) => {
                let k_id = self.from_ty(k);
                let v_id = self.from_ty(v);
                self.app(TypeId::MAP, smallvec![k_id, v_id])
            }
            Ty::Tuple(elems) => {
                let elem_ids: SmallVec<[_; 4]> =
                    elems.iter().map(|e| self.from_ty(e)).collect();
                self.tuple(elem_ids)
            }
            Ty::Named(type_id, params) => {
                if params.is_empty() {
                    self.named(*type_id)
                } else {
                    let param_ids: SmallVec<[_; 2]> =
                        params.iter().map(|p| self.from_ty(p)).collect();
                    self.app(*type_id, param_ids)
                }
            }
            Ty::Fn(params, ret) => {
                let param_ids: SmallVec<[_; 4]> =
                    params.iter().map(|p| self.from_ty(p)).collect();
                let ret_id = self.from_ty(ret);
                self.fn_type(param_ids, ret_id)
            }
            Ty::Object(fields) => {
                let converted: IndexMap<StringId, TypeExprId> =
                    fields.iter().map(|(k, t)| (*k, self.from_ty(t))).collect();
                self.object(converted)
            }
            Ty::Union(members) => {
                let member_ids: SmallVec<[_; 4]> =
                    members.iter().map(|m| self.from_ty(m)).collect();
                self.union(member_ids)
            }
            Ty::Var(_) | Ty::Unknown | Ty::Error => {
                unreachable!("from_ty called on unresolved type: {ty:?}")
            }
        }
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
    pub(crate) fn new(
        arena: &mut ValueArena,
        type_exprs: &mut TypeExprArena,
    ) -> Result<Self> {
        let mut reg = Self {
            defs: Vec::new(),
            by_name: HashMap::new(),
        };
        reg.register_builtins(arena, type_exprs)?;
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
            TypeDef::Sum { name, .. }
            | TypeDef::Struct { name, .. }
            | TypeDef::Union { name, .. } => arena.get_str(*name),
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
            TypeDef::Builtin(_)
            | TypeDef::Struct { .. }
            | TypeDef::Union { .. } => None,
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
            TypeDef::Builtin(_)
            | TypeDef::Struct { .. }
            | TypeDef::Union { .. } => None,
            TypeDef::Sum { variants, .. } => {
                variants.iter().find(|v| v.name == name)
            }
        })
    }

    /// Register all built-in types (called from `new`).
    ///
    /// Registers in order: Bool, Int, Float, String, Array, Object, Option, Result, Char,
    /// Tuple, Map, Time, Range, Unit, Json, Storable, Scalar.
    fn register_builtins(
        &mut self,
        arena: &mut ValueArena,
        type_exprs: &mut TypeExprArena,
    ) -> Result<()> {
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

        // Object at index 5: registered internally but NOT user-accessible.
        // Users should use structural object types: `{ field: Type, ... }`
        self.defs.push(TypeDef::Builtin(BuiltinType::Object));
        // NOTE: No by_name insert; users cannot reference "Object" in annotations.

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
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: some_name,
                        idx: 1,
                        arity: 1,
                        // Builtin types don't use AST type expressions for payloads;
                        // the type checker handles Option/Result specially via Ty::Option/Ty::Result
                        payloads: SmallVec::new(),
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
                        arity: 1,
                        // Builtin: type checker handles Result specially
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: err_name,
                        idx: 1,
                        arity: 1,
                        // Builtin: type checker handles Result specially
                        payloads: SmallVec::new(),
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

        // Tuple at index 9 (registered for type lookup, though Tuple types use
        // TypeExpr::Tuple rather than TypeExpr::App)
        let tuple_name = arena.intern("Tuple");
        let tup =
            self.register(TypeDef::Builtin(BuiltinType::Tuple), tuple_name);
        (tup == TypeId::TUPLE).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Tuple at index {}, expected {}",
                tup.0,
                TypeId::TUPLE.0
            ))
        })?;

        // Map[K, V] at index 10
        let map_name = arena.intern("Map");
        let map = self.register(TypeDef::Builtin(BuiltinType::Map), map_name);
        (map == TypeId::MAP).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Map at index {}, expected {}",
                map.0,
                TypeId::MAP.0
            ))
        })?;

        // Time at index 11
        let time_name = arena.intern("Time");
        let time =
            self.register(TypeDef::Builtin(BuiltinType::Time), time_name);
        (time == TypeId::TIME).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Time at index {}, expected {}",
                time.0,
                TypeId::TIME.0
            ))
        })?;

        // Range at index 12
        let range_name = arena.intern("Range");
        let range =
            self.register(TypeDef::Builtin(BuiltinType::Range), range_name);
        (range == TypeId::RANGE).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Range at index {}, expected {}",
                range.0,
                TypeId::RANGE.0
            ))
        })?;

        // Unit at index 13
        let unit_name = arena.intern("Unit");
        let unit =
            self.register(TypeDef::Builtin(BuiltinType::Unit), unit_name);
        (unit == TypeId::UNIT).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Unit at index {}, expected {}",
                unit.0,
                TypeId::UNIT.0
            ))
        })?;

        // Json at index 14
        let json_name = arena.intern("Json");
        let json =
            self.register(TypeDef::Builtin(BuiltinType::Json), json_name);
        (json == TypeId::JSON).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Json at index {}, expected {}",
                json.0,
                TypeId::JSON.0
            ))
        })?;

        // Storable union at index 15: Bool | Int | Float | Char | String | Json
        let storable_name = arena.intern("Storable");
        let storable_members: SmallVec<[TypeExprId; 8]> = smallvec::smallvec![
            type_exprs.named(TypeId::BOOL),
            type_exprs.named(TypeId::INT),
            type_exprs.named(TypeId::FLOAT),
            type_exprs.named(TypeId::CHAR),
            type_exprs.named(TypeId::STRING),
            type_exprs.named(TypeId::JSON),
        ];
        let storable = self.register(
            TypeDef::Union {
                name: storable_name,
                type_params: SmallVec::new(),
                members: storable_members,
            },
            storable_name,
        );
        (storable == TypeId::STORABLE)
            .then_some(())
            .ok_or_else(|| {
                crate::Error::runtime_no_span(format!(
                    "Storable at index {}, expected {}",
                    storable.0,
                    TypeId::STORABLE.0
                ))
            })?;

        // Scalar union at index 16: Bool | Int | Float | String
        let scalar_name = arena.intern("Scalar");
        let scalar_members: SmallVec<[TypeExprId; 8]> = smallvec::smallvec![
            type_exprs.named(TypeId::BOOL),
            type_exprs.named(TypeId::INT),
            type_exprs.named(TypeId::FLOAT),
            type_exprs.named(TypeId::STRING),
        ];
        let scalar = self.register(
            TypeDef::Union {
                name: scalar_name,
                type_params: SmallVec::new(),
                members: scalar_members,
            },
            scalar_name,
        );
        (scalar == TypeId::SCALAR).then_some(()).ok_or_else(|| {
            crate::Error::runtime_no_span(format!(
                "Scalar at index {}, expected {}",
                scalar.0,
                TypeId::SCALAR.0
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
        let mut type_exprs = TypeExprArena::new();
        let reg = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();

        // 13 primitives + Option + Result + Storable + Scalar = 17
        assert_eq!(reg.len(), 17);

        let bool_name = arena.intern("Bool");
        let option_name = arena.intern("Option");
        let result_name = arena.intern("Result");
        let storable_name = arena.intern("Storable");
        let scalar_name = arena.intern("Scalar");

        assert!(reg.lookup(bool_name).is_some());
        assert_eq!(reg.lookup(option_name), Some(TypeId::OPTION));
        assert_eq!(reg.lookup(result_name), Some(TypeId::RESULT));
        assert_eq!(reg.lookup(storable_name), Some(TypeId::STORABLE));
        assert_eq!(reg.lookup(scalar_name), Some(TypeId::SCALAR));
    }

    #[test]
    fn option_values() {
        let mut arena = ValueArena::new();
        let mut type_exprs = TypeExprArena::new();
        let _reg = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();

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
        let mut type_exprs = TypeExprArena::new();
        let _reg = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();

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
    fn variant_name_lookup() {
        let mut arena = ValueArena::new();
        let mut type_exprs = TypeExprArena::new();
        let reg = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();

        assert_eq!(reg.variant_name(TypeId::OPTION, 0, &arena), Some("None"));
        assert_eq!(reg.variant_name(TypeId::OPTION, 1, &arena), Some("Some"));

        assert_eq!(reg.variant_name(TypeId::RESULT, 0, &arena), Some("Ok"));
        assert_eq!(reg.variant_name(TypeId::RESULT, 1, &arena), Some("Err"));
    }
}
