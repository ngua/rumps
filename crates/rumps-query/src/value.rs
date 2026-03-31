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
use std::fmt;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use crate::ast::{
    Ast, AstTypeExprId, ExprId, Stmt, StmtId, TypeDefAst, TypeParam,
};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::typecheck::{Ty, TyArena, TyId};
use crate::Span;

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
    /// `AS Storable` is infallible; `AS` to other unions requires `read`.
    pub(crate) const STORABLE: Self = Self(15);
    /// Builtin union: `Scalar = Bool | Int | Float | String`.
    ///
    /// Used as return type for `->>`  JSON scalar extraction.
    pub(crate) const SCALAR: Self = Self(16);
    /// Builtin enum: `Ordering = Lt | Eq | Gt`.
    ///
    /// Used for comparison results in `sort-by` and similar operations.
    pub(crate) const ORDERING: Self = Self(17);
    /// Builtin opaque type: `FilePath`.
    ///
    /// Represents a file system path. Created from `String` via coercion.
    pub(crate) const FILEPATH: Self = Self(18);
    /// Builtin enum: `Path = File(FilePath) | Dir(FilePath)`.
    ///
    /// Represents a file system entry (file or directory).
    pub(crate) const PATH: Self = Self(19);
    /// Builtin opaque type: `Regex`.
    ///
    /// Represents a compiled regular expression pattern.
    pub(crate) const REGEX: Self = Self(20);
    /// Builtin enum: `DataStatus = NoData | HasValue | HasDescendants | Both`.
    ///
    /// Result of `data` primitive; indicates node existence status.
    pub(crate) const DATA_STATUS: Self = Self(21);
    /// Builtin union: `Subscript = Bool | Int | Float | Char | String | Json`.
    ///
    /// The set of types that can be used as subscripts in variable references.
    /// Semantically distinct from `Storable` (what can be stored) though identical
    /// in terms of actual representation.
    pub(crate) const SUBSCRIPT: Self = Self(22);
    /// Runtime error type: `Error.Runtime(msg)`, `Error.Raise(msg)`, etc.
    pub(crate) const ERROR: Self = Self(23);
    /// Builtin type: `Word` (unsigned machine word).
    ///
    /// An unsigned integral type mapping to `usize`. Satisfies `Numeric` constraint.
    pub(crate) const WORD: Self = Self(24);
    /// Builtin type: `Local`.
    ///
    /// A local database variable reference (e.g., `data{1, 2}`).
    pub(crate) const LOCAL: Self = Self(25);
    /// Builtin type: `Global`.
    ///
    /// A global database variable reference (e.g., `^info{key}`).
    pub(crate) const GLOBAL: Self = Self(26);
    /// Builtin union: `Ref = Local | Global`.
    ///
    /// A database reference that can be either local or global.
    pub(crate) const REF: Self = Self(27);
    /// Placeholder type for uninferred type parameters; compatible with any type.
    /// Used for empty arrays (unknown element type) and partial variant types
    /// (e.g., `Option.None` has unknown `T`, `Result.Ok(v)` has unknown `E`).
    /// Note: This is NOT a registered type; it's a marker used in type expressions.
    pub(crate) const UNKNOWN: Self = Self(u32::MAX);

    /// User-accessible builtin `TypeId`s (excludes `OBJECT` and `UNKNOWN`).
    ///
    /// Must be kept in sync with `name()`.
    pub(crate) const ALL_BUILTINS: &[Self] = &[
        Self::BOOL,
        Self::INT,
        Self::FLOAT,
        Self::STRING,
        Self::ARRAY,
        Self::OPTION,
        Self::RESULT,
        Self::CHAR,
        Self::TUPLE,
        Self::MAP,
        Self::TIME,
        Self::RANGE,
        Self::UNIT,
        Self::JSON,
        Self::STORABLE,
        Self::SCALAR,
        Self::ORDERING,
        Self::FILEPATH,
        Self::PATH,
        Self::REGEX,
        Self::DATA_STATUS,
        Self::SUBSCRIPT,
        Self::ERROR,
        Self::WORD,
        Self::LOCAL,
        Self::GLOBAL,
        Self::REF,
    ];

    /// Returns the canonical name for builtin types, or `None` for
    /// non-user-accessible types (`OBJECT`, `UNKNOWN`) and user-defined types.
    pub(crate) const fn name(self) -> Option<&'static str> {
        match self.0 {
            0 => Some("Bool"),
            1 => Some("Int"),
            2 => Some("Float"),
            3 => Some("String"),
            4 => Some("Array"),
            6 => Some("Option"),
            7 => Some("Result"),
            8 => Some("Char"),
            9 => Some("Tuple"),
            10 => Some("Map"),
            11 => Some("Time"),
            12 => Some("Range"),
            13 => Some("Unit"),
            14 => Some("Json"),
            15 => Some("Storable"),
            16 => Some("Scalar"),
            17 => Some("Ordering"),
            18 => Some("FilePath"),
            19 => Some("Path"),
            20 => Some("Regex"),
            21 => Some("DataStatus"),
            22 => Some("Subscript"),
            23 => Some("Error"),
            24 => Some("Word"),
            25 => Some("Local"),
            26 => Some("Global"),
            27 => Some("Ref"),
            _ => None,
        }
    }

    const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Index into the class registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub(crate) struct ClassId(u32);

impl ClassId {
    pub(crate) const fn new(v: u32) -> Self {
        Self(v)
    }

    pub(crate) const NUMERIC: Self = Self(0);
    pub(crate) const ITERABLE: Self = Self(1);
    pub(crate) const MONOID: Self = Self(2);
    pub(crate) const BIT_LIKE: Self = Self(3);
    pub(crate) const NEGATABLE: Self = Self(4);
    pub(crate) const FALLIBLE: Self = Self(5);
    pub(crate) const INTO: Self = Self(6);
    pub(crate) const TRY_INTO: Self = Self(7);
    pub(crate) const INDEXABLE: Self = Self(8);
    pub(crate) const ORD: Self = Self(9);
    pub(crate) const MAPPABLE: Self = Self(10);
    pub(crate) const FOLDABLE: Self = Self(11);
    pub(crate) const FILTERABLE: Self = Self(12);
    pub(crate) const DISPLAY: Self = Self(13);
    pub(crate) const EQ: Self = Self(14);
    pub(crate) const WRAPPABLE: Self = Self(15);
    pub(crate) const CHAINABLE: Self = Self(16);

    pub(crate) const BUILTIN_COUNT: usize = 17;

    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }

    /// Returns the name of builtin classes as a `&'static str`.
    pub(crate) const fn name(self) -> &'static str {
        match self.0 {
            0 => "Numeric",
            1 => "Iterable",
            2 => "Monoid",
            3 => "BitLike",
            4 => "Negatable",
            5 => "Fallible",
            6 => "Into",
            7 => "TryInto",
            8 => "Indexable",
            9 => "Ord",
            10 => "Mappable",
            11 => "Foldable",
            12 => "Filterable",
            13 => "Display",
            14 => "Eq",
            15 => "Wrappable",
            16 => "Chainable",
            _ => "<unknown class>",
        }
    }

    pub(crate) fn from_name(s: &str) -> Option<Self> {
        match s {
            "Numeric" => Some(Self::NUMERIC),
            "Iterable" => Some(Self::ITERABLE),
            "Monoid" => Some(Self::MONOID),
            "BitLike" => Some(Self::BIT_LIKE),
            "Negatable" => Some(Self::NEGATABLE),
            "Fallible" => Some(Self::FALLIBLE),
            "Into" => Some(Self::INTO),
            "TryInto" => Some(Self::TRY_INTO),
            "Indexable" => Some(Self::INDEXABLE),
            "Ord" => Some(Self::ORD),
            "Mappable" => Some(Self::MAPPABLE),
            "Foldable" => Some(Self::FOLDABLE),
            "Filterable" => Some(Self::FILTERABLE),
            "Display" => Some(Self::DISPLAY),
            "Eq" => Some(Self::EQ),
            "Wrappable" => Some(Self::WRAPPABLE),
            "Chainable" => Some(Self::CHAINABLE),
            _ => None,
        }
    }
}

impl fmt::Display for ClassId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
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

    /// Create a value arena with a pre-populated string interner.
    pub(crate) fn with_interner(interner: StringInterner) -> Self {
        Self {
            values: Vec::new(),
            value_spans: Vec::new(),
            strings: interner,
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
    /// Unwraps Union/Newtype wrappers to find the inner String.
    pub(crate) fn get_string_id(&self, id: ValueId) -> Option<StringId> {
        match self.get(id)? {
            Value::String(sid) => Some(*sid),
            Value::Union(_, inner) | Value::Newtype(_, inner) => {
                self.get_string_id(*inner)
            }
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
    /// single-arm `if` (side-effect only).
    Unit,

    /// A boolean value.
    Bool(bool),

    /// A 64-bit integer.
    Int(i64),

    /// An unsigned machine word (`usize`).
    Word(usize),

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

    /// An opaque file path.
    ///
    /// Created from `String` via type coercion or `Io.Directory` functions.
    /// Used with `Io.Directory` module for file system operations.
    FilePath(StringId),

    /// A compiled regular expression.
    ///
    /// Created from regex literals (`/pattern/`). The pattern is validated
    /// and compiled during typechecking; at runtime we just retrieve the
    /// pre-compiled regex by its cache index.
    Regex(u32),

    /// A tagged value (sum type variant).
    ///
    /// - `TypeExprId`: the full parameterized type (e.g., `Option[Int]`, `Result[Int, String]`)
    /// - `u8`: the variant index (e.g., `0` for `None`, `1` for `Some`)
    /// - `SmallVec`: the payload values (most variants have 0-4)
    Tagged(TypeExprId, u8, SmallVec<[ValueId; 4]>),

    /// A value in a union type context.
    ///
    /// - `TypeExprId`: identifies the union type (e.g., `Storable`, `Int | String`)
    /// - `ValueId`: the underlying value
    ///
    /// Used for both named unions (`TypeDef::Union`) and inline unions (`TypeExpr::Union`).
    /// Class methods dispatch to the inner value's type implementation unless
    /// a user-defined instance exists for the union type itself.
    Union(TypeExprId, ValueId),

    /// A value in a newtype context.
    ///
    /// - `TypeExprId`: identifies the newtype (e.g., `UserId` if `newtype UserId = Int`)
    /// - `ValueId`: the underlying value
    ///
    /// Class methods auto-derive by delegating to the wrapped type unless
    /// a user-defined instance exists for the newtype.
    Newtype(TypeExprId, ValueId),

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
    /// Created when a module path like `String.length` is evaluated. Can be
    /// called directly or used as a first-class value (e.g., in pipelines).
    ///
    /// The path includes the full module path plus function name:
    /// - `String.length` -> `["String", "length"]`
    /// - `Math.Trig.sin` -> `["Math", "Trig", "sin"]`
    ModuleFn { path: SmallVec<[StringId; 4]> },

    /// A class method reference.
    ///
    /// Created when a class method like `Filterable:filter` is evaluated
    /// without a call. Can be called later or used as a first-class value.
    ///
    /// - `class`: the class name (e.g., `"Filterable"`)
    /// - `method`: the method name (e.g., `"filter"`)
    /// - `expr_id`: for convert methods (`wrap`, `into`, `try-into`), the
    ///   expression ID of the `ClassMethodRef` so the interpreter can look up
    ///   the target type from `convert_targets`
    ClassMethodFn {
        class: StringId,
        method: StringId,
        expr_id: Option<ExprId>,
    },

    /// A module constant reference.
    ///
    /// Created when a module constant like `Math.pi` is imported. The actual
    /// value is looked up from `Environment::consts` at evaluation time.
    ///
    /// The path includes the full module path plus constant name:
    /// - `Math.pi` -> `["Math", "pi"]`
    ModuleConst { path: SmallVec<[StringId; 4]> },

    /// A lazy integer range.
    ///
    /// Created by `start..end` (exclusive) or `start..=end` (inclusive).
    /// Does not allocate; used with collection operations like `Iter.map`.
    ///
    /// - `start`: the first value in the range
    /// - `end`: the bound (exclusive or inclusive depending on `inclusive`)
    /// - `inclusive`: `true` for `..=`, `false` for `..`
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },

    /// A FOREVER loop continuation pseudo-function.
    ///
    /// Not a real callable; calling this triggers loop continuation in the
    /// interpreter.
    ForeverContinuation,

    /// Signal to continue a FOREVER loop with a new state.
    ///
    /// This is never exposed to user code; it's an internal signal between
    /// the continuation call and the FOREVER loop interpreter. The `ValueId`
    /// points to the new state value.
    LoopContinue(ValueId),

    /// A database reference (local or global variable with subscripts).
    ///
    /// Created via `data{1, 2}` or `^global{key}` syntax.
    /// Used with intrinsics: `@get r`, `@set r value`, etc.
    ///
    /// - `bool`: `true` for global (`^var`), `false` for local
    /// - `StringId`: the variable name
    /// - `SmallVec`: subscript values (already evaluated)
    Ref(bool, StringId, SmallVec<[ValueId; 4]>),
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
            Self::Word(_) => Cow::Borrowed("Word"),
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
            Self::FilePath(_) => Cow::Borrowed("FilePath"),
            Self::Regex(_) => Cow::Borrowed("Regex"),
            Self::Tagged(ty_expr, _, _) => Cow::Borrowed(
                type_exprs
                    .base_type(*ty_expr)
                    .and_then(|ty| reg.get_def(ty))
                    .map(|def| match def {
                        TypeDef::Builtin(b) => b.name(),
                        TypeDef::Sum { .. } => "Tagged",
                        TypeDef::Alias { .. } => "Alias",
                        TypeDef::Union { .. } => "Union",
                    })
                    .unwrap_or("Unknown"),
            ),
            Self::Union(..) => Cow::Borrowed("Union"),
            Self::Newtype(..) => Cow::Borrowed("Newtype"),
            Self::Closure { .. } => Cow::Borrowed("Closure"),
            Self::Function { .. } => Cow::Borrowed("Function"),
            Self::ModuleFn { .. } => Cow::Borrowed("ModuleFn"),
            Self::ModuleConst { .. } => Cow::Borrowed("ModuleConst"),
            Self::Range { .. } => Cow::Borrowed("Range"),
            Self::ForeverContinuation => Cow::Borrowed("Continuation"),
            Self::LoopContinue(_) => Cow::Borrowed("LoopContinue"),
            Self::Ref(..) => Cow::Borrowed("Ref"),
            Self::ClassMethodFn { .. } => Cow::Borrowed("ClassMethodFn"),
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

    /// Create an `Ordering.Lt` value with the given type expression.
    pub(crate) fn lt(ty_expr: TypeExprId) -> Self {
        Self::Tagged(ty_expr, 0, SmallVec::new())
    }

    /// Create an `Ordering.Eq` value with the given type expression.
    pub(crate) fn eq_ord(ty_expr: TypeExprId) -> Self {
        Self::Tagged(ty_expr, 1, SmallVec::new())
    }

    /// Create an `Ordering.Gt` value with the given type expression.
    pub(crate) fn gt(ty_expr: TypeExprId) -> Self {
        Self::Tagged(ty_expr, 2, SmallVec::new())
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
            Self::Word(_) => TypeId::WORD,
            Self::Float(_) => TypeId::FLOAT,
            Self::Char(_) => TypeId::CHAR,
            Self::String(_) => TypeId::STRING,
            Self::Array(..) => TypeId::ARRAY,
            Self::Object(_) => TypeId::OBJECT,
            Self::Tuple(..) => TypeId::TUPLE,
            Self::Map(..) => TypeId::MAP,
            Self::Time(_) => TypeId::TIME,
            Self::Json(_) => TypeId::JSON,
            Self::FilePath(_) => TypeId::FILEPATH,
            Self::Regex(_) => TypeId::REGEX,
            Self::Tagged(ty_expr, _, _)
            | Self::Union(ty_expr, _)
            | Self::Newtype(ty_expr, _) => {
                type_exprs.base_type(*ty_expr).unwrap_or(TypeId::UNKNOWN)
            }
            Self::Closure { .. }
            | Self::Function { .. }
            | Self::ModuleFn { .. }
            | Self::ModuleConst { .. }
            | Self::ClassMethodFn { .. } => TypeId::UNKNOWN,
            Self::Range { .. } => TypeId::RANGE,
            Self::Ref(..) => TypeId::REF,
            // Internal types; not exposed to user code
            Self::ForeverContinuation | Self::LoopContinue(_) => TypeId::UNIT,
        }
    }
}

/// Built-in primitive types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinType {
    Bool,
    Int,
    Word,
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
    FilePath,
    Regex,
    Local,
    Global,
}

impl BuiltinType {
    const fn name(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Int => "Int",
            Self::Word => "Word",
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
            Self::FilePath => "FilePath",
            Self::Regex => "Regex",
            Self::Local => "Local",
            Self::Global => "Global",
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
    /// Transparent type alias.
    ///
    /// `newtype I = Int` makes `I` fully interchangeable with `Int`.
    /// The target is stored as an AST type expression to support
    /// type parameters; resolution happens at usage site with substitution.
    Alias {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        /// The target type (AST expression, not resolved).
        target: AstTypeExprId,
    },
    /// Named union type definition.
    ///
    /// Union types represent a value that can be one of several types.
    /// Used for `union Storable = Bool | Int | ...` declarations.
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
                // Single-element tuples need trailing comma: `(Int,)`
                let trail = if elems.len() == 1 { "," } else { "" };
                format!("({parts}{trail})")
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
    pub(crate) fn intern_ty(&mut self, id: TyId, ta: &TyArena) -> TypeExprId {
        match ta.get(id) {
            Ty::Bool => self.named(TypeId::BOOL),
            Ty::Int => self.named(TypeId::INT),
            Ty::Word => self.named(TypeId::WORD),
            Ty::Float => self.named(TypeId::FLOAT),
            Ty::Char => self.named(TypeId::CHAR),
            Ty::String => self.named(TypeId::STRING),
            Ty::Unit => self.named(TypeId::UNIT),
            Ty::Time => self.named(TypeId::TIME),
            Ty::Range => self.named(TypeId::RANGE),
            Ty::Json => self.named(TypeId::JSON),
            Ty::Ordering => self.named(TypeId::ORDERING),
            Ty::DataStatus => self.named(TypeId::DATA_STATUS),
            Ty::FilePath => self.named(TypeId::FILEPATH),
            Ty::Path => self.named(TypeId::PATH),
            Ty::Regex => self.named(TypeId::REGEX),
            Ty::RuntimeError => self.named(TypeId::ERROR),
            Ty::Local => self.named(TypeId::LOCAL),
            Ty::Global => self.named(TypeId::GLOBAL),
            Ty::Array(elem) => {
                let elem_id = self.intern_ty(*elem, ta);
                self.app(TypeId::ARRAY, smallvec![elem_id])
            }
            Ty::Option(inner) => {
                let inner_id = self.intern_ty(*inner, ta);
                self.app(TypeId::OPTION, smallvec![inner_id])
            }
            Ty::Result(ok, err) => {
                let ok_id = self.intern_ty(*ok, ta);
                let err_id = self.intern_ty(*err, ta);
                self.app(TypeId::RESULT, smallvec![ok_id, err_id])
            }
            Ty::Map(k, v) => {
                let k_id = self.intern_ty(*k, ta);
                let v_id = self.intern_ty(*v, ta);
                self.app(TypeId::MAP, smallvec![k_id, v_id])
            }
            Ty::Tuple(elems) => {
                let elems = elems.clone();
                let elem_ids: SmallVec<[_; 4]> =
                    elems.iter().map(|&e| self.intern_ty(e, ta)).collect();
                self.tuple(elem_ids)
            }
            Ty::Named(type_id, params) => {
                if params.is_empty() {
                    self.named(*type_id)
                } else {
                    let params = params.clone();
                    let param_ids: SmallVec<[_; 2]> =
                        params.iter().map(|&p| self.intern_ty(p, ta)).collect();
                    self.app(*type_id, param_ids)
                }
            }
            Ty::Fn(params, ret) => {
                let params = params.clone();
                let ret = *ret;
                let param_ids: SmallVec<[_; 4]> =
                    params.iter().map(|&p| self.intern_ty(p, ta)).collect();
                let ret_id = self.intern_ty(ret, ta);
                self.fn_type(param_ids, ret_id)
            }
            Ty::Object(fields) => {
                let fields = fields.clone();
                let converted: IndexMap<StringId, TypeExprId> = fields
                    .iter()
                    .map(|(&k, &t)| (k, self.intern_ty(t, ta)))
                    .collect();
                self.object(converted)
            }
            Ty::Union(_, members) => {
                let members = members.clone();
                let member_ids: SmallVec<[_; 4]> =
                    members.iter().map(|&m| self.intern_ty(m, ta)).collect();
                self.union(member_ids)
            }
            Ty::Var(_)
            | Ty::Apply(_, _)
            | Ty::AssocType(_, _, _)
            | Ty::Unknown
            | Ty::Error => {
                unreachable!("intern_ty called on unresolved type: {id:?}")
            }
        }
    }

    /// Convert a `Ty` to a runtime `TypeExprId`, substituting `UNKNOWN` for
    /// unresolved type variables.
    ///
    /// Use this when the type may contain type variables (e.g., from generics
    /// that haven't been monomorphized). For empty containers, the element
    /// type doesn't matter at runtime.
    pub(crate) fn intern_ty_lenient(
        &mut self,
        id: TyId,
        ta: &TyArena,
    ) -> TypeExprId {
        match ta.get(id) {
            Ty::Bool => self.named(TypeId::BOOL),
            Ty::Int => self.named(TypeId::INT),
            Ty::Word => self.named(TypeId::WORD),
            Ty::Float => self.named(TypeId::FLOAT),
            Ty::Char => self.named(TypeId::CHAR),
            Ty::String => self.named(TypeId::STRING),
            Ty::Unit => self.named(TypeId::UNIT),
            Ty::Time => self.named(TypeId::TIME),
            Ty::Range => self.named(TypeId::RANGE),
            Ty::Json => self.named(TypeId::JSON),
            Ty::Ordering => self.named(TypeId::ORDERING),
            Ty::DataStatus => self.named(TypeId::DATA_STATUS),
            Ty::FilePath => self.named(TypeId::FILEPATH),
            Ty::Path => self.named(TypeId::PATH),
            Ty::Regex => self.named(TypeId::REGEX),
            Ty::RuntimeError => self.named(TypeId::ERROR),
            Ty::Local => self.named(TypeId::LOCAL),
            Ty::Global => self.named(TypeId::GLOBAL),
            Ty::Array(elem) => {
                let elem_id = self.intern_ty_lenient(*elem, ta);
                self.app(TypeId::ARRAY, smallvec![elem_id])
            }
            Ty::Option(inner) => {
                let inner_id = self.intern_ty_lenient(*inner, ta);
                self.app(TypeId::OPTION, smallvec![inner_id])
            }
            Ty::Result(ok, err) => {
                let ok_id = self.intern_ty_lenient(*ok, ta);
                let err_id = self.intern_ty_lenient(*err, ta);
                self.app(TypeId::RESULT, smallvec![ok_id, err_id])
            }
            Ty::Map(k, v) => {
                let k_id = self.intern_ty_lenient(*k, ta);
                let v_id = self.intern_ty_lenient(*v, ta);
                self.app(TypeId::MAP, smallvec![k_id, v_id])
            }
            Ty::Tuple(elems) => {
                let elems = elems.clone();
                let elem_ids: SmallVec<[_; 4]> = elems
                    .iter()
                    .map(|&e| self.intern_ty_lenient(e, ta))
                    .collect();
                self.tuple(elem_ids)
            }
            Ty::Named(type_id, params) => {
                if params.is_empty() {
                    self.named(*type_id)
                } else {
                    let params = params.clone();
                    let param_ids: SmallVec<[_; 2]> = params
                        .iter()
                        .map(|&p| self.intern_ty_lenient(p, ta))
                        .collect();
                    self.app(*type_id, param_ids)
                }
            }
            Ty::Fn(params, ret) => {
                let params = params.clone();
                let ret = *ret;
                let param_ids: SmallVec<[_; 4]> = params
                    .iter()
                    .map(|&p| self.intern_ty_lenient(p, ta))
                    .collect();
                let ret_id = self.intern_ty_lenient(ret, ta);
                self.fn_type(param_ids, ret_id)
            }
            Ty::Object(fields) => {
                let fields = fields.clone();
                let converted: IndexMap<StringId, TypeExprId> = fields
                    .iter()
                    .map(|(&k, &t)| (k, self.intern_ty_lenient(t, ta)))
                    .collect();
                self.object(converted)
            }
            Ty::Union(_, members) => {
                let members = members.clone();
                let member_ids: SmallVec<[_; 4]> = members
                    .iter()
                    .map(|&m| self.intern_ty_lenient(m, ta))
                    .collect();
                self.union(member_ids)
            }
            // Unresolved types become UNKNOWN
            Ty::Var(_)
            | Ty::Apply(_, _)
            | Ty::AssocType(_, _, _)
            | Ty::Unknown
            | Ty::Error => self.named(TypeId::UNKNOWN),
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
    by_name: HashMap<QualifiedName, TypeId>,
}

/// Context for union type registration.
struct UnionRegCtx<'a> {
    arena: &'a mut ValueArena,
    type_exprs: &'a mut TypeExprArena,
    ast: &'a Ast,
}

impl TypeRegistry {
    /// Create a type registry with all builtins registered.
    pub(crate) fn new(
        arena: &mut ValueArena,
        type_exprs: &mut TypeExprArena,
    ) -> Self {
        let mut reg = Self {
            defs: Vec::new(),
            by_name: HashMap::new(),
        };
        reg.register_builtins(arena, type_exprs);
        reg
    }

    pub(crate) fn register(
        &mut self,
        def: TypeDef,
        name: QualifiedName,
    ) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
        self.by_name.insert(name, id);
        self.defs.push(def);
        id
    }

    pub(crate) fn get_def(&self, id: TypeId) -> Option<&TypeDef> {
        self.defs.get(id.idx())
    }

    /// Look up a type by its qualified name.
    pub(crate) fn lookup(&self, name: &QualifiedName) -> Option<TypeId> {
        self.by_name.get(name).copied()
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
            | TypeDef::Alias { name, .. }
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
            | TypeDef::Alias { .. }
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
            | TypeDef::Alias { .. }
            | TypeDef::Union { .. } => None,
            TypeDef::Sum { variants, .. } => {
                variants.iter().find(|v| v.name == name)
            }
        })
    }

    /// Get the number of type parameters for a type.
    ///
    /// Returns `None` for builtin types (handled separately).
    pub(crate) fn type_param_count(&self, id: TypeId) -> Option<usize> {
        self.get_def(id).and_then(|def| match def {
            TypeDef::Builtin(_) => None,
            TypeDef::Sum { type_params, .. }
            | TypeDef::Alias { type_params, .. }
            | TypeDef::Union { type_params, .. } => Some(type_params.len()),
        })
    }

    /// Get the registered name for a type by reverse lookup in `by_name`.
    ///
    /// Returns the name used to register the type, which may differ from
    /// the display name returned by `type_name()`.
    ///
    /// Note: This uses a linear scan over registered types. If this becomes
    /// a hot path, consider adding a reverse lookup cache (`TypeId` -> `StringId`).
    pub(crate) fn name<'a>(
        &self,
        id: TypeId,
        arena: &'a ValueArena,
    ) -> Option<&'a str> {
        self.by_name
            .iter()
            .find(|(_, &tid)| tid == id)
            .and_then(|(qn, _)| arena.get_str(qn.local_name()))
    }

    /// Register all built-in types (called from `new`).
    ///
    /// Registers in order: Bool, Int, Float, String, Array, Object, Option, Result, Char,
    /// Tuple, Map, Time, Range, Unit, Json, Storable, Scalar.
    fn register_builtins(
        &mut self,
        arena: &mut ValueArena,
        type_exprs: &mut TypeExprArena,
    ) {
        // Primitives (indices 0-5)
        let bool_name = arena.intern("Bool");
        self.register(TypeDef::Builtin(BuiltinType::Bool), bool_name.into());

        let int_name = arena.intern("Int");
        self.register(TypeDef::Builtin(BuiltinType::Int), int_name.into());

        let float_name = arena.intern("Float");
        self.register(TypeDef::Builtin(BuiltinType::Float), float_name.into());

        let string_name = arena.intern("String");
        self.register(
            TypeDef::Builtin(BuiltinType::String),
            string_name.into(),
        );

        let array_name = arena.intern("Array");
        self.register(TypeDef::Builtin(BuiltinType::Array), array_name.into());

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
            option_name.into(),
        );
        if opt != TypeId::OPTION {
            invariant!("Option registered at expected index");
        }

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
            result_name.into(),
        );
        if res != TypeId::RESULT {
            invariant!("Result registered at expected index");
        }

        // Char at index 8
        let char_name = arena.intern("Char");
        let ch = self
            .register(TypeDef::Builtin(BuiltinType::Char), char_name.into());
        if ch != TypeId::CHAR {
            invariant!("Char registered at expected index");
        }

        // Tuple at index 9 (registered for type lookup, though Tuple types use
        // TypeExpr::Tuple rather than TypeExpr::App)
        let tuple_name = arena.intern("Tuple");
        let tup = self
            .register(TypeDef::Builtin(BuiltinType::Tuple), tuple_name.into());
        if tup != TypeId::TUPLE {
            invariant!("Tuple registered at expected index");
        }

        // Map[K, V] at index 10
        let map_name = arena.intern("Map");
        let map =
            self.register(TypeDef::Builtin(BuiltinType::Map), map_name.into());
        if map != TypeId::MAP {
            invariant!("Map registered at expected index");
        }

        // Time at index 11
        let time_name = arena.intern("Time");
        let time = self
            .register(TypeDef::Builtin(BuiltinType::Time), time_name.into());
        if time != TypeId::TIME {
            invariant!("Time registered at expected index");
        }

        // Range at index 12
        let range_name = arena.intern("Range");
        let range = self
            .register(TypeDef::Builtin(BuiltinType::Range), range_name.into());
        if range != TypeId::RANGE {
            invariant!("Range registered at expected index");
        }

        // Unit at index 13
        let unit_name = arena.intern("Unit");
        let unit = self
            .register(TypeDef::Builtin(BuiltinType::Unit), unit_name.into());
        if unit != TypeId::UNIT {
            invariant!("Unit registered at expected index");
        }

        // Json at index 14
        let json_name = arena.intern("Json");
        let json = self
            .register(TypeDef::Builtin(BuiltinType::Json), json_name.into());
        if json != TypeId::JSON {
            invariant!("Json registered at expected index");
        }

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
            storable_name.into(),
        );
        if storable != TypeId::STORABLE {
            invariant!("Storable registered at expected index");
        }

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
            scalar_name.into(),
        );
        if scalar != TypeId::SCALAR {
            invariant!("Scalar registered at expected index");
        }

        // Ordering at index 17
        let ordering_name = arena.intern("Ordering");
        let lt_name = arena.intern("Lt");
        let eq_name = arena.intern("Eq");
        let gt_name = arena.intern("Gt");

        let ordering = self.register(
            TypeDef::Sum {
                name: ordering_name,
                type_params: SmallVec::new(),
                variants: smallvec::smallvec![
                    VariantDef {
                        name: lt_name,
                        idx: 0,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: eq_name,
                        idx: 1,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: gt_name,
                        idx: 2,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                ],
            },
            ordering_name.into(),
        );
        if ordering != TypeId::ORDERING {
            invariant!("Ordering registered at expected index");
        }

        // FilePath at index 18
        let filepath_name = arena.intern("FilePath");
        let filepath = self.register(
            TypeDef::Builtin(BuiltinType::FilePath),
            filepath_name.into(),
        );
        if filepath != TypeId::FILEPATH {
            invariant!("FilePath registered at expected index");
        }

        // Path at index 19: File(FilePath) | Dir(FilePath)
        let path_name = arena.intern("Path");
        let file_name = arena.intern("File");
        let dir_name = arena.intern("Dir");

        let path = self.register(
            TypeDef::Sum {
                name: path_name,
                type_params: SmallVec::new(),
                variants: smallvec::smallvec![
                    VariantDef {
                        name: file_name,
                        idx: 0,
                        arity: 1,
                        // Builtin: type checker handles Path specially
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: dir_name,
                        idx: 1,
                        arity: 1,
                        // Builtin: type checker handles Path specially
                        payloads: SmallVec::new(),
                    },
                ],
            },
            path_name.into(),
        );
        if path != TypeId::PATH {
            invariant!("Path registered at expected index");
        }

        // Regex at index 20
        let regex_name = arena.intern("Regex");
        let regex = self
            .register(TypeDef::Builtin(BuiltinType::Regex), regex_name.into());
        if regex != TypeId::REGEX {
            invariant!("Regex registered at expected index");
        }

        // DataStatus at index 21: NoData | HasValue | HasDescendants | Both
        let data_status_name = arena.intern("DataStatus");
        let no_data = arena.intern("NoData");
        let has_value = arena.intern("HasValue");
        let has_descendants = arena.intern("HasDescendants");
        let both = arena.intern("Both");

        let data_status = self.register(
            TypeDef::Sum {
                name: data_status_name,
                type_params: SmallVec::new(),
                variants: smallvec::smallvec![
                    VariantDef {
                        name: no_data,
                        idx: 0,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: has_value,
                        idx: 1,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: has_descendants,
                        idx: 2,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: both,
                        idx: 3,
                        arity: 0,
                        payloads: SmallVec::new(),
                    },
                ],
            },
            data_status_name.into(),
        );
        if data_status != TypeId::DATA_STATUS {
            invariant!("DataStatus registered at expected index");
        }

        // Subscript union at index 22: Bool | Int | Float | Char | String | Json
        let subscript_name = arena.intern("Subscript");
        let subscript_members: SmallVec<[TypeExprId; 8]> = smallvec::smallvec![
            type_exprs.named(TypeId::BOOL),
            type_exprs.named(TypeId::INT),
            type_exprs.named(TypeId::FLOAT),
            type_exprs.named(TypeId::CHAR),
            type_exprs.named(TypeId::STRING),
            type_exprs.named(TypeId::JSON),
        ];
        let subscript = self.register(
            TypeDef::Union {
                name: subscript_name,
                type_params: SmallVec::new(),
                members: subscript_members,
            },
            subscript_name.into(),
        );
        if subscript != TypeId::SUBSCRIPT {
            invariant!("Subscript registered at expected index");
        }

        // Error at index 23: Runtime(String) | Raise(String) | Type(String) | Coerce(String)
        let error_name = arena.intern("Error");
        let runtime_name = arena.intern("Runtime");
        let raise_name = arena.intern("Raise");
        let type_name = arena.intern("Type");
        let coerce_name = arena.intern("Coerce");

        let error = self.register(
            TypeDef::Sum {
                name: error_name,
                type_params: SmallVec::new(),
                variants: smallvec::smallvec![
                    VariantDef {
                        name: runtime_name,
                        idx: 0,
                        arity: 1,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: raise_name,
                        idx: 1,
                        arity: 1,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: type_name,
                        idx: 2,
                        arity: 1,
                        payloads: SmallVec::new(),
                    },
                    VariantDef {
                        name: coerce_name,
                        idx: 3,
                        arity: 1,
                        payloads: SmallVec::new(),
                    },
                ],
            },
            error_name.into(),
        );
        if error != TypeId::ERROR {
            invariant!("Error registered at expected index");
        }

        // Word at index 24
        let word_name = arena.intern("Word");
        let word = self
            .register(TypeDef::Builtin(BuiltinType::Word), word_name.into());
        if word != TypeId::WORD {
            invariant!("Word registered at expected index");
        }

        // Local at index 25
        let local_name = arena.intern("Local");
        let local_ty = self
            .register(TypeDef::Builtin(BuiltinType::Local), local_name.into());
        if local_ty != TypeId::LOCAL {
            invariant!("Local registered at expected index");
        }

        // Global at index 26
        let global_name = arena.intern("Global");
        let global_ty = self.register(
            TypeDef::Builtin(BuiltinType::Global),
            global_name.into(),
        );
        if global_ty != TypeId::GLOBAL {
            invariant!("Global registered at expected index");
        }

        // Ref union at index 27: Local | Global
        let ref_name = arena.intern("Ref");
        let ref_members: SmallVec<[TypeExprId; 8]> = smallvec::smallvec![
            type_exprs.named(TypeId::LOCAL),
            type_exprs.named(TypeId::GLOBAL),
        ];
        let ref_ty = self.register(
            TypeDef::Union {
                name: ref_name,
                type_params: SmallVec::new(),
                members: ref_members,
            },
            ref_name.into(),
        );
        if ref_ty != TypeId::REF {
            invariant!("Ref registered at expected index");
        }
    }

    /// Pre-register user-defined types from AST before type checking.
    ///
    /// Scans all statements for `type` and `union` declarations (including
    /// those inside modules) and registers them so the type checker can
    /// resolve type names. Module-scoped types are registered with qualified
    /// names (e.g., `MyModule.MyType`).
    pub(crate) fn register_from_ast(
        &mut self,
        ast: &Ast,
        stmts: &[StmtId],
        arena: &mut ValueArena,
        type_exprs: &mut TypeExprArena,
    ) {
        let mut ctx = UnionRegCtx {
            arena,
            type_exprs,
            ast,
        };
        self.register_stmts_with_prefix(stmts, None, &mut ctx);
    }

    /// Register types from statements with an optional module path prefix.
    ///
    /// Recursively descends into modules, tracking the qualified name prefix.
    fn register_stmts_with_prefix(
        &mut self,
        stmts: &[StmtId],
        prefix: Option<&QualifiedName>,
        ctx: &mut UnionRegCtx,
    ) {
        stmts.iter().for_each(|id| {
            ctx.ast.get_stmt(*id).cloned().inspect(|stmt| match stmt {
                Stmt::Type {
                    name,
                    type_params,
                    def,
                    ..
                } => {
                    let qn = prefix.map_or_else(
                        || QualifiedName::local(*name),
                        |p| p.child(*name),
                    );
                    self.register_type(qn, type_params, def, ctx.arena);
                }
                Stmt::Union {
                    name,
                    type_params,
                    members,
                    ..
                } => {
                    let qn = prefix.map_or_else(
                        || QualifiedName::local(*name),
                        |p| p.child(*name),
                    );
                    self.register_union(qn, type_params, members, ctx);
                }
                Stmt::NewType {
                    name,
                    type_params,
                    target,
                    ..
                } => {
                    let qn = prefix.map_or_else(
                        || QualifiedName::local(*name),
                        |p| p.child(*name),
                    );
                    self.register_alias(qn, type_params, *target, ctx.arena);
                }
                Stmt::Module { name, body } => {
                    let new_prefix = prefix.map_or_else(
                        || QualifiedName::local(*name),
                        |p| p.child(*name),
                    );
                    self.register_stmts_with_prefix(
                        body,
                        Some(&new_prefix),
                        ctx,
                    );
                }
                _ => {}
            });
        });
    }

    /// Register a single TYPE declaration.
    ///
    /// If a type with the same name already exists, it is shadowed.
    fn register_type(
        &mut self,
        qn: QualifiedName,
        type_params: &[TypeParam],
        def: &TypeDefAst,
        arena: &mut ValueArena,
    ) {
        let disp = qn.display(&arena.strings);
        let name_id = arena.intern(&disp);

        // Type parameters already have `StringId`; use directly
        let type_param_ids: SmallVec<[StringId; 2]> =
            type_params.iter().map(|tp| tp.name).collect();

        let TypeDefAst::Sum(variants) = def;
        let variant_defs: SmallVec<[VariantDef; 4]> = variants
            .iter()
            .enumerate()
            .map(|(idx, v)| VariantDef {
                name: v.name,
                idx: idx as u8,
                arity: v.payloads.len() as u8,
                payloads: v.payloads.clone(),
            })
            .collect();

        self.register(
            TypeDef::Sum {
                name: name_id,
                type_params: type_param_ids,
                variants: variant_defs,
            },
            qn.clone(),
        );
    }

    /// Register a single union declaration.
    ///
    /// If a type with the same name already exists, it is shadowed.
    fn register_union(
        &mut self,
        qn: QualifiedName,
        type_params: &[TypeParam],
        ast_members: &[AstTypeExprId],
        ctx: &mut UnionRegCtx,
    ) {
        let disp = qn.display(&ctx.arena.strings);
        let name_id = ctx.arena.intern(&disp);

        // Type parameters already have `StringId`; use directly
        let type_param_ids: SmallVec<[StringId; 2]> =
            type_params.iter().map(|tp| tp.name).collect();

        // Convert AST type expressions to TypeExprIds
        let members: SmallVec<[TypeExprId; 8]> = ast_members
            .iter()
            .map(|m| {
                resolve_type_expr(ctx.ast, ctx.arena, self, ctx.type_exprs, *m)
            })
            .collect();

        self.register(
            TypeDef::Union {
                name: name_id,
                type_params: type_param_ids,
                members,
            },
            qn.clone(),
        );
    }

    /// Register a single newtype alias declaration.
    ///
    /// If a type with the same name already exists, it is shadowed.
    fn register_alias(
        &mut self,
        qn: QualifiedName,
        type_params: &[TypeParam],
        target: AstTypeExprId,
        arena: &mut ValueArena,
    ) {
        let disp = qn.display(&arena.strings);
        let name_id = arena.intern(&disp);

        // Type parameters already have `StringId`; use directly
        let type_param_ids: SmallVec<[StringId; 2]> =
            type_params.iter().map(|tp| tp.name).collect();

        self.register(
            TypeDef::Alias {
                name: name_id,
                type_params: type_param_ids,
                target,
            },
            qn.clone(),
        );
    }

    fn len(&self) -> usize {
        self.defs.len()
    }

    fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Resolve an AST type expression to a `TypeExprId`.
///
/// Used during type registration to convert user type annotations.
fn resolve_type_expr(
    ast: &Ast,
    arena: &mut ValueArena,
    registry: &TypeRegistry,
    type_exprs: &mut TypeExprArena,
    id: AstTypeExprId,
) -> TypeExprId {
    use crate::ast::AstTypeExpr;

    let te = ast
        .get_type_expr(id)
        .unwrap_or_else(|| invariant!("AST type expression ID exists"));

    match te {
        // Wildcard shouldn't appear in runtime type resolution
        AstTypeExpr::Wildcard => {
            typechecked!("type resolution", "no wildcard at runtime")
        }
        AstTypeExpr::Named(name) => {
            let ty_id = registry.lookup(name).unwrap_or_else(|| {
                typechecked!("type reference", "type is defined")
            });
            type_exprs.named(ty_id)
        }
        AstTypeExpr::App(name, args) => {
            let base = registry.lookup(name).unwrap_or_else(|| {
                typechecked!("type reference", "type is defined")
            });
            let arg_ids: SmallVec<[TypeExprId; 2]> = args
                .iter()
                .map(|a| {
                    resolve_type_expr(ast, arena, registry, type_exprs, *a)
                })
                .collect();
            type_exprs.app(base, arg_ids)
        }
        AstTypeExpr::Tuple(elems) => {
            let elem_ids: SmallVec<[TypeExprId; 4]> = elems
                .iter()
                .map(|e| {
                    resolve_type_expr(ast, arena, registry, type_exprs, *e)
                })
                .collect();
            type_exprs.tuple(elem_ids)
        }
        AstTypeExpr::Fn(params, ret) => {
            let param_ids: SmallVec<[TypeExprId; 4]> = params
                .iter()
                .map(|p| {
                    resolve_type_expr(ast, arena, registry, type_exprs, *p)
                })
                .collect();
            let ret_id =
                resolve_type_expr(ast, arena, registry, type_exprs, *ret);
            type_exprs.fn_type(param_ids, ret_id)
        }
        AstTypeExpr::Union(members) => {
            let member_ids: SmallVec<[TypeExprId; 4]> = members
                .iter()
                .map(|m| {
                    resolve_type_expr(ast, arena, registry, type_exprs, *m)
                })
                .collect();
            type_exprs.union(member_ids)
        }
        AstTypeExpr::Object(fields) => {
            let field_ids: IndexMap<StringId, TypeExprId> = fields
                .iter()
                .map(|(name, ty)| {
                    let ty_id = resolve_type_expr(
                        ast, arena, registry, type_exprs, *ty,
                    );
                    (*name, ty_id)
                })
                .collect();
            type_exprs.object(field_ids)
        }
        // `VarApp` shouldn't appear in union member registration; it means
        // an HKT type param is used as a constructor, which is unresolvable
        // at registration time.
        AstTypeExpr::VarApp(..) => {
            invariant!("`VarApp` in union member registration")
        }
        // Associated types should be resolved by typechecker before runtime
        AstTypeExpr::AssocType { .. } => {
            typechecked!("type resolution", "associated types resolved")
        }
    }
}
