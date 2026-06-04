//! Runtime value types with arena allocation and string interning.
//!
//! Uses arena allocation for cache efficiency and to avoid `Box` in recursive
//! structures. Strings are interned to avoid duplication and enable O(1)
//! comparison. A type registry enables runtime type validation, `is` checks,
//! and clear error messages.
//!
//! # Clone cost
//!
//! Cloning a [`Value`] is shallow for large heap-backed data. Scalar payload
//! variants are `Copy`-sized, and collection variants (`Array`, `Object`,
//! `Tuple`, `Map`) plus `Json` wrap their heap data in `Arc`, so cloning large
//! collections is a refcount bump. Variants, refs, partial applications, and
//! callables may copy small inline vectors of `ValueId` or metadata, but they do
//! not deep-copy nested values.
//!
//! For mutation sites that need owned inner data (e.g. `Array.push`), use the
//! `take_array` / `take_map` accessors on [`ValueArena`]; these use
//! `Arc::unwrap_or_clone` to avoid a deep copy when the refcount is `1`.
//!
//! # Note
//!
//! Payload conversion methods (to/from storage, JSON, display) live on
//! `Interpreter` rather than `Payload` because they require context (arena,
//! registry) that the interpreter owns.

#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use itertools::Itertools;
use ordered_float::OrderedFloat;
use smallvec::{smallvec, SmallVec};

use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, ExprId, Stmt, StmtId, TypeDefAst,
    TypeParam,
};
use crate::intern::{QualifiedName, StringId, StringInterner};
use crate::typecheck::RuntimeTyId;
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
    /// Convert a `Payload` to a `MapKey`, or `None` if not a scalar type.
    pub(crate) fn from_payload(v: &Payload) -> Option<Self> {
        match v {
            Payload::Bool(b) => Some(Self::Bool(*b)),
            Payload::Int(n) => Some(Self::Int(*n)),
            Payload::Float(f) => Some(Self::Float(*f)),
            Payload::Char(c) => Some(Self::Char(*c)),
            Payload::String(sid) => Some(Self::String(*sid)),
            _ => None,
        }
    }

    /// Convert a `MapKey` back to a `Payload`.
    pub(crate) fn to_payload(&self) -> Payload {
        match self {
            Self::Bool(b) => Payload::Bool(*b),
            Self::Int(n) => Payload::Int(*n),
            Self::Float(f) => Payload::Float(*f),
            Self::Char(c) => Payload::Char(*c),
            Self::String(sid) => Payload::String(*sid),
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
    /// `as Storable` is infallible; `as` to other unions requires `read`.
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
    /// User-accessible builtin `TypeId`s, excludes `OBJECT`.
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
    /// non-user-accessible types (`OBJECT`) and user-defined types.
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
    pub(crate) const DEFAULT: Self = Self(2);
    pub(crate) const CONCATABLE: Self = Self(3);
    pub(crate) const BIT_LIKE: Self = Self(4);
    pub(crate) const NEGATABLE: Self = Self(5);
    pub(crate) const FALLIBLE: Self = Self(6);
    pub(crate) const INTO: Self = Self(7);
    pub(crate) const TRY_INTO: Self = Self(8);
    pub(crate) const INDEXABLE: Self = Self(9);
    pub(crate) const ORD: Self = Self(10);
    pub(crate) const MAPPABLE: Self = Self(11);
    pub(crate) const FOLDABLE: Self = Self(12);
    pub(crate) const FILTERABLE: Self = Self(13);
    pub(crate) const DISPLAY: Self = Self(14);
    pub(crate) const EQ: Self = Self(15);
    pub(crate) const WRAPPABLE: Self = Self(16);
    pub(crate) const CHAINABLE: Self = Self(17);
    pub(crate) const BIMAPPABLE: Self = Self(18);
    pub(crate) const ADDITIVE: Self = Self(19);
    pub(crate) const SUBTRACTIVE: Self = Self(20);
    pub(crate) const MULTIPLICATIVE: Self = Self(21);
    pub(crate) const DIVISIBLE: Self = Self(22);
    pub(crate) const FLOOR_DIVISIBLE: Self = Self(23);
    pub(crate) const POWERABLE: Self = Self(24);

    pub(crate) const BUILTIN_COUNT: usize = 25;

    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }

    /// Returns the name of builtin classes as a `&'static str`.
    ///
    /// User-defined classes (id `>= BUILTIN_COUNT`) return `"<user class>"`;
    /// use `ClassRegistry::name()` to get the actual name via the string arena.
    pub(crate) const fn name(self) -> &'static str {
        match self.0 {
            0 => "Numeric",
            1 => "Iterable",
            2 => "Default",
            3 => "Concatable",
            4 => "BitLike",
            5 => "Negatable",
            6 => "Fallible",
            7 => "Into",
            8 => "TryInto",
            9 => "Indexable",
            10 => "Ord",
            11 => "Mappable",
            12 => "Foldable",
            13 => "Filterable",
            14 => "Display",
            15 => "Eq",
            16 => "Wrappable",
            17 => "Chainable",
            18 => "Bimappable",
            19 => "Additive",
            20 => "Subtractive",
            21 => "Multiplicative",
            22 => "Divisible",
            23 => "FloorDivisible",
            24 => "Powerable",
            _ => "<user class>",
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

    /// Add a typed value.
    pub(crate) fn add(&mut self, v: Value, span: Span) -> ValueId {
        let id = ValueId(self.values.len() as u32);
        self.values.push(v);
        self.value_spans.push(span);
        id
    }

    /// Add a payload with explicit type metadata.
    pub(crate) fn add_typed(
        &mut self,
        payload: Payload,
        meta: ValueMeta,
        span: Span,
    ) -> ValueId {
        self.add(
            Value {
                ty: meta.ty,
                repr: meta.repr,
                payload,
            },
            span,
        )
    }

    /// Get the type metadata for a value.
    pub(crate) fn meta(&self, id: ValueId) -> Option<ValueMeta> {
        self.values.get(id.idx()).map(|v| ValueMeta {
            ty: v.ty,
            repr: v.repr,
        })
    }

    /// Get a value by ID.
    pub(crate) fn get(&self, id: ValueId) -> Option<&Value> {
        self.values.get(id.idx())
    }

    /// Get a value by ID.
    pub(crate) fn value(&self, id: ValueId) -> Option<&Value> {
        self.get(id)
    }

    /// Get a payload by ID.
    pub(crate) fn payload(&self, id: ValueId) -> Option<&Payload> {
        self.values.get(id.idx()).map(|v| &v.payload)
    }

    /// Get the semantic type for a value.
    pub(crate) fn ty(&self, id: ValueId) -> Option<RuntimeTyId> {
        self.values.get(id.idx()).map(|v| v.ty)
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

    /// Get array elements by ID (no cloning; returns a reference into the `Arc`).
    ///
    /// Returns `None` if the value doesn't exist or isn't an array.
    pub(crate) fn get_array(
        &self,
        id: ValueId,
    ) -> Option<&SmallVec<[ValueId; 4]>> {
        match self.payload(id)? {
            Payload::Array(elems) => Some(elems),
            _ => None,
        }
    }

    /// Get owned array elements by ID, avoiding a clone when the `Arc`
    /// refcount is `1`.
    ///
    /// Use this instead of `get_array` at mutation sites (push, pop, etc.)
    /// where you need a mutable `SmallVec`.
    pub(crate) fn take_array(
        &self,
        id: ValueId,
    ) -> Option<SmallVec<[ValueId; 4]>> {
        match self.payload(id)? {
            Payload::Array(elems) => Some(Arc::unwrap_or_clone(elems.clone())),
            _ => None,
        }
    }

    /// Get object fields by ID (no cloning; returns a reference into the `Arc`).
    ///
    /// Returns `None` if the value doesn't exist or isn't an object.
    pub(crate) fn get_object(
        &self,
        id: ValueId,
    ) -> Option<&IndexMap<StringId, ValueId>> {
        match self.payload(id)? {
            Payload::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Get tuple elements by ID (no cloning; returns a reference into the `Arc`).
    ///
    /// Returns `None` if the value doesn't exist or isn't a tuple.
    pub(crate) fn get_tuple(
        &self,
        id: ValueId,
    ) -> Option<&SmallVec<[ValueId; 4]>> {
        match self.payload(id)? {
            Payload::Tuple(elems) => Some(elems),
            _ => None,
        }
    }

    /// Get string ID from a value, returning the interned `StringId`.
    ///
    /// Returns `None` if the value doesn't exist or isn't a string.
    pub(crate) fn get_string_id(&self, id: ValueId) -> Option<StringId> {
        match self.payload(id)? {
            Payload::String(sid) => Some(*sid),
            _ => None,
        }
    }

    /// Get map contents by ID (no cloning; returns a reference into the `Arc`).
    ///
    /// Returns `None` if the value doesn't exist or isn't a map.
    pub(crate) fn get_map(
        &self,
        id: ValueId,
    ) -> Option<&IndexMap<MapKey, ValueId>> {
        match self.payload(id)? {
            Payload::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// Get owned map contents by ID, avoiding a clone when the `Arc`
    /// refcount is `1`.
    ///
    /// Use this instead of `get_map` at mutation sites (insert, remove, etc.)
    /// where you need a mutable `IndexMap`.
    pub(crate) fn take_map(
        &self,
        id: ValueId,
    ) -> Option<IndexMap<MapKey, ValueId>> {
        match self.payload(id)? {
            Payload::Map(entries) => {
                Some(Arc::unwrap_or_clone(entries.clone()))
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

/// The runtime payload of a value (data without type metadata).
///
/// Uses `StringId` for interned strings and `ValueId` for nested values,
/// avoiding allocation and enabling O(1) string comparison.
///
/// Collection variants (`Array`, `Object`, `Tuple`, `Map`) and `Json`/`Closure`
/// wrap their heap data in `Arc`, so cloning large nested data is shallow.
/// Callers that need owned inner data should use `Arc::unwrap_or_clone()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Payload {
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

    /// An array of values. Note that in native RUMPS arrays
    /// elements must be homogeneous.
    Array(Arc<SmallVec<[ValueId; 4]>>),

    /// An object/record with string keys (insertion order preserved).
    Object(Arc<IndexMap<StringId, ValueId>>),

    /// A tuple value (heterogeneous, fixed-size sequence).
    ///
    /// Unlike arrays, tuples can hold different types and support positional
    /// access (`.0`, `.1`, etc.).
    Tuple(Arc<SmallVec<[ValueId; 4]>>),

    /// A homogeneous map with typed keys and values.
    ///
    /// Keys are restricted to scalar types (Bool, Int, Float, Char, String).
    /// Insertion order is preserved.
    Map(Arc<IndexMap<MapKey, ValueId>>),

    /// A point in time (UTC).
    Time(DateTime<Utc>),

    /// An opaque JSON value.
    ///
    /// Wraps `serde_json::Value`. JSON values are created from:
    /// - Object literals with quoted keys: `{ "id": 123 }`
    /// - Heterogeneous array literals: `[1, "two", true]`
    /// - Explicit cast: `value as Json`
    ///
    /// Access via `.` and `->` returns `Json`; `..` and `->>` extract scalars.
    Json(Arc<serde_json::Value>),

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

    /// A sum type variant.
    ///
    /// Type identity lives on `Value.ty`; payload data keeps only the variant
    /// tag and child values.
    Variant {
        tag: u8,
        vals: SmallVec<[ValueId; 4]>,
    },

    /// A payload-taking variant constructor used as a function value.
    VariantCtor { ty: QualifiedName, var: StringId },

    /// A closure (anonymous function) with captured environment.
    ///
    /// Closures capture their lexical scope at creation time by value.
    /// The body is an AST expression ID; the interpreter evaluates it
    /// with the captured environment restored when the closure is called.
    Closure {
        params: SmallVec<[(StringId, RuntimeTyId); 4]>,
        ret: RuntimeTyId,
        body: ExprId,
        env: Arc<CapturedEnv>,
    },

    /// A named function reference.
    ///
    /// When a named function is referenced without being called (e.g., `f` instead
    /// of `f(x)`), it produces this value. This enables passing functions to
    /// higher-order functions.
    Function {
        name: StringId,
        params: SmallVec<[(StringId, RuntimeTyId); 4]>,
        ret: RuntimeTyId,
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
    /// - `expr_id`: the expression ID of the `ClassMethodRef` so the
    ///   interpreter can look up checked expression metadata
    ClassMethodFn {
        class: StringId,
        method: StringId,
        expr_id: Option<ExprId>,
    },

    /// A partially applied function.
    ///
    /// Created when a callable is invoked with fewer arguments than its arity.
    /// The `callee` is the original callable (stored in the arena); `bound`
    /// holds the already-supplied arguments. When enough args accumulate the
    /// original callable is invoked with all args. Chained partial application
    /// always references the original callee (no nesting).
    PartialApp {
        callee: ValueId,
        bound: SmallVec<[ValueId; 4]>,
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

    /// A `loop` continuation pseudo-function.
    ///
    /// Not a real callable; calling this triggers loop continuation in the
    /// interpreter.
    LoopContinuation,

    /// Signal to continue a `loop` expression with a new state.
    ///
    /// This is never exposed to user code; it's an internal signal between
    /// the continuation call and the `loop` interpreter. The `ValueId`
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

impl Payload {
    /// Get the type name of this value for error messages.
    ///
    /// Returns a structural type representation for objects (e.g., `{ name: String }`).
    /// Other types return their simple names.
    pub(crate) fn type_name(
        &self,
        reg: &TypeRegistry,
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
                let parts = fields
                    .iter()
                    .filter_map(|(name_id, val_id)| {
                        let name = arena.get_str(*name_id)?;
                        let val = arena.payload(*val_id)?;
                        let ty = val.type_name(reg, arena);
                        Some(format!("{name}: {ty}"))
                    })
                    .join(", ");
                Cow::Owned(format!("{{ {parts} }}"))
            }
            Self::Tuple(..) => Cow::Borrowed("Tuple"),
            Self::Map(..) => Cow::Borrowed("Map"),
            Self::Time(_) => Cow::Borrowed("Time"),
            Self::Json(_) => Cow::Borrowed("Json"),
            Self::FilePath(_) => Cow::Borrowed("FilePath"),
            Self::Regex(_) => Cow::Borrowed("Regex"),
            Self::Variant { .. } => Cow::Borrowed("Variant"),
            Self::VariantCtor { .. } => Cow::Borrowed("VariantCtor"),
            Self::Closure { .. } => Cow::Borrowed("Closure"),
            Self::Function { .. } => Cow::Borrowed("Function"),
            Self::ModuleFn { .. } => Cow::Borrowed("ModuleFn"),
            Self::ModuleConst { .. } => Cow::Borrowed("ModuleConst"),
            Self::Range { .. } => Cow::Borrowed("Range"),
            Self::LoopContinuation => Cow::Borrowed("Continuation"),
            Self::LoopContinue(_) => Cow::Borrowed("LoopContinue"),
            Self::Ref(..) => Cow::Borrowed("Ref"),
            Self::ClassMethodFn { .. } => Cow::Borrowed("ClassMethodFn"),
            Self::PartialApp { .. } => Cow::Borrowed("PartialApp"),
        }
    }

    /// Create an `Option.None` value.
    pub(crate) fn none() -> Self {
        Self::Variant {
            tag: 0,
            vals: SmallVec::new(),
        }
    }

    /// Create an `Option.Some(v)` value.
    pub(crate) fn some(v: ValueId) -> Self {
        Self::Variant {
            tag: 1,
            vals: smallvec![v],
        }
    }

    /// Create a `Result.Ok(v)` value.
    pub(crate) fn ok(v: ValueId) -> Self {
        Self::Variant {
            tag: 0,
            vals: smallvec![v],
        }
    }

    /// Create a `Result.Err(e)` value.
    pub(crate) fn err(e: ValueId) -> Self {
        Self::Variant {
            tag: 1,
            vals: smallvec![e],
        }
    }

    /// Create an `Ordering.Lt` value.
    pub(crate) fn lt() -> Self {
        Self::Variant {
            tag: 0,
            vals: SmallVec::new(),
        }
    }

    /// Create an `Ordering.Eq` value.
    pub(crate) fn eq_ord() -> Self {
        Self::Variant {
            tag: 1,
            vals: SmallVec::new(),
        }
    }

    /// Create an `Ordering.Gt` value.
    pub(crate) fn gt() -> Self {
        Self::Variant {
            tag: 2,
            vals: SmallVec::new(),
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
    Alias {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
    },
    /// Named union type definition.
    ///
    /// Union types represent a value that can be one of several types.
    /// Used for `union Storable = Bool | Int | ...` declarations.
    /// At runtime, `is` checks test against each member; `as` casts are
    /// infallible only for `Storable` (special-cased).
    Union {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        /// Member base types.
        members: SmallVec<[TypeId; 8]>,
    },
}

/// A named function definition stored in the function registry.
///
/// Created from `Stmt::Fun` during interpretation; the names are
/// resolved to interned IDs.
#[derive(Clone, Debug)]
pub(crate) struct FunctionDef {
    pub(crate) name: StringId,
    pub(crate) params: SmallVec<[(StringId, RuntimeTyId); 4]>,
    pub(crate) ret: RuntimeTyId,
    pub(crate) body: ExprId,
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
    ast: &'a Ast,
}

impl TypeRegistry {
    /// Create a type registry with all builtins registered.
    pub(crate) fn new(arena: &mut ValueArena) -> Self {
        let mut reg = Self {
            defs: Vec::new(),
            by_name: HashMap::new(),
        };
        reg.register_builtins(arena);
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

    fn register_internal(&mut self, def: TypeDef) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
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

    /// Find all registered sum types that define a variant with `name`.
    pub(crate) fn lookup_variant_types(
        &self,
        name: StringId,
    ) -> Vec<(TypeId, QualifiedName)> {
        self.by_name
            .iter()
            .filter_map(|(qn, &id)| match self.get_def(id) {
                Some(TypeDef::Sum { variants, .. })
                    if variants.iter().any(|v| v.name == name) =>
                {
                    Some((id, qn.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Get the number of type parameters for a type.
    pub(crate) fn type_param_count(&self, id: TypeId) -> Option<usize> {
        self.get_def(id)
            .and_then(|def| match def {
                TypeDef::Builtin(_) => None,
                TypeDef::Sum { type_params, .. }
                | TypeDef::Alias { type_params, .. }
                | TypeDef::Union { type_params, .. } => Some(type_params.len()),
            })
            .or_else(|| Self::builtin_type_param_count(id))
    }

    /// Type parameter count for builtin parameterized types.
    fn builtin_type_param_count(id: TypeId) -> Option<usize> {
        match id {
            TypeId::ARRAY | TypeId::OPTION => Some(1),
            TypeId::RESULT | TypeId::MAP => Some(2),
            _ => None,
        }
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
    fn register_builtins(&mut self, arena: &mut ValueArena) {
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
        self.register_internal(TypeDef::Builtin(BuiltinType::Object));
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
                    },
                    VariantDef {
                        name: some_name,
                        idx: 1,
                        arity: 1,
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
                    },
                    VariantDef {
                        name: err_name,
                        idx: 1,
                        arity: 1,
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

        // Tuple at index 9; registered for type lookup.
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
        let storable_members: SmallVec<[TypeId; 8]> = smallvec::smallvec![
            TypeId::BOOL,
            TypeId::INT,
            TypeId::FLOAT,
            TypeId::CHAR,
            TypeId::STRING,
            TypeId::JSON,
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
        let scalar_members: SmallVec<[TypeId; 8]> = smallvec::smallvec![
            TypeId::BOOL,
            TypeId::INT,
            TypeId::FLOAT,
            TypeId::STRING,
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
                    },
                    VariantDef {
                        name: eq_name,
                        idx: 1,
                        arity: 0,
                    },
                    VariantDef {
                        name: gt_name,
                        idx: 2,
                        arity: 0,
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
                    },
                    VariantDef {
                        name: dir_name,
                        idx: 1,
                        arity: 1,
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
                    },
                    VariantDef {
                        name: has_value,
                        idx: 1,
                        arity: 0,
                    },
                    VariantDef {
                        name: has_descendants,
                        idx: 2,
                        arity: 0,
                    },
                    VariantDef {
                        name: both,
                        idx: 3,
                        arity: 0,
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
        let subscript_members: SmallVec<[TypeId; 8]> = smallvec::smallvec![
            TypeId::BOOL,
            TypeId::INT,
            TypeId::FLOAT,
            TypeId::CHAR,
            TypeId::STRING,
            TypeId::JSON,
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
                    },
                    VariantDef {
                        name: raise_name,
                        idx: 1,
                        arity: 1,
                    },
                    VariantDef {
                        name: type_name,
                        idx: 2,
                        arity: 1,
                    },
                    VariantDef {
                        name: coerce_name,
                        idx: 3,
                        arity: 1,
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
        let ref_members: SmallVec<[TypeId; 8]> =
            smallvec::smallvec![TypeId::LOCAL, TypeId::GLOBAL,];
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
    /// Scans all statements for `variant` and `union` declarations (including
    /// those inside modules) and registers them so the type checker can
    /// resolve type names. Module-scoped types are registered with qualified
    /// names (e.g., `MyModule.MyType`).
    pub(crate) fn register_from_ast(
        &mut self,
        ast: &Ast,
        stmts: &[StmtId],
        arena: &mut ValueArena,
    ) {
        let mut ctx = UnionRegCtx { arena, ast };
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
                Stmt::Newtype {
                    name, type_params, ..
                } => {
                    let qn = prefix.map_or_else(
                        || QualifiedName::local(*name),
                        |p| p.child(*name),
                    );
                    self.register_alias(qn, type_params, ctx.arena);
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

    /// Register a single `variant` declaration.
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

        let type_param_ids: SmallVec<[StringId; 2]> =
            type_params.iter().map(|tp| tp.name).collect();

        let members: SmallVec<[TypeId; 8]> = ast_members
            .iter()
            .map(|m| self.resolve_member_type(ctx.ast, *m))
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

    /// Resolve an AST type expression to a base `TypeId` for union member registration.
    ///
    /// Union members are stored as `TypeId`s, so we only need
    /// the base type name lookup.
    fn resolve_member_type(&self, ast: &Ast, id: AstTypeExprId) -> TypeId {
        let te = ast
            .get_type_expr(id)
            .unwrap_or_else(|| invariant!("AST type expression ID exists"));

        match te {
            AstTypeExpr::Named(name) | AstTypeExpr::App(name, _) => {
                self.lookup(name).unwrap_or_else(|| {
                    typechecked!("type reference", "type is defined")
                })
            }
            _ => typechecked!("union member", "named type expected"),
        }
    }
}

/// A runtime value: typed envelope over a `Payload`.
///
/// `ty` is the semantic type; `repr` is the transparent representation type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Value<P = Payload> {
    pub(crate) ty: RuntimeTyId,
    pub(crate) repr: RuntimeTyId,
    pub(crate) payload: P,
}

/// Lightweight metadata pairing a solved type with its representation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ValueMeta {
    pub(crate) ty: RuntimeTyId,
    pub(crate) repr: RuntimeTyId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typecheck::TyArena;

    const _: fn(&mut ValueArena, Value, Span) -> ValueId = ValueArena::add;
    const _: fn(&mut ValueArena, Payload, ValueMeta, Span) -> ValueId =
        ValueArena::add_typed;

    #[test]
    fn phase_11_value_arena_stores_full_value_metadata() {
        let mut vals = ValueArena::new();
        let ty = RuntimeTyId::from(TyArena::INT);
        let repr = RuntimeTyId::from(TyArena::FLOAT);
        let id = vals.add(
            Value {
                ty,
                repr,
                payload: Payload::Int(7),
            },
            Span::default(),
        );

        assert_eq!(vals.ty(id), Some(ty));
        assert_eq!(vals.meta(id), Some(ValueMeta { ty, repr }));
        assert_eq!(vals.get(id).map(|v| v.repr), Some(repr));
    }
}
