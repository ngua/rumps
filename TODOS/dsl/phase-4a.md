# Phase 4: Static Type System

Add a Hindley-Milner style type inference and checking phase. The type checker runs after name resolution but before interpretation, rejecting programs with type errors at compile time.

## Design Decisions

| Decision         | Choice                                   | Rationale                                                                               |
|------------------|------------------------------------------|-----------------------------------------------------------------------------------------|
| DB operations    | Infer from usage, fallback to `Storable` | `LET x = $GET local(1); x + 1` infers `Int`; ambiguous cases resolve to `Storable` union |
| Object typing    | Structural                               | Objects compatible if they have required fields                                         |
| Error handling   | Reject at compile                        | Type errors prevent execution                                                           |
| Numeric coercion | Float result                             | `Int + Float = Float` (widening); `Int -> Float` only, not bidirectional                |
| Type erasure     | None                                     | Runtime type info preserved for `IS` operator                                           |
| Recursive types  | Not supported                            | No use case in query language; simplifies inference                                     |
| Mutability       | None                                     | All bindings immutable; no value restriction needed for let-generalization              |
| Variance         | Invariant                                | No subtyping hierarchy; `Int -> Float` coercion is explicit, not subtyping              |

### Variance: Why Invariant?

Variance governs how subtyping of type parameters affects subtyping of generic types. In languages with class hierarchies (e.g., `Cat <: Animal`), this matters:

```java
// Java: Arrays are covariant (unsound!)
Animal[] animals = new Cat[10];  // allowed
animals[0] = new Dog();          // runtime error: Dog into Cat[]
```

RUMPS avoids this entirely:

1. **No subtyping hierarchy**: There's no `Cat <: Animal` relationship
2. **No mutability**: Even if we had subtyping, immutable containers are safe
3. **Numeric coercion is not subtyping**: `Int + Float = Float` is an operation rule, not `Int <: Float`

Therefore, all generic types (`Array[T]`, `Option[T]`, `Map[K, V]`, etc.) are **invariant**:

```rumps
LET ints: Array[Int] = [1, 2, 3]
LET floats: Array[Float] = ints    ; ERROR: Array[Int] != Array[Float]

; Explicit conversion if needed
LET floats: Array[Float] = ints |> Array.map(n => (n) : Float)
```

This is the simplest correct choice. Variance annotations or inference can be added later if a compelling use case emerges.

## Pipeline Integration

```
Lexer -> CST -> AST -> Name Resolution -> [TYPE CHECK] -> Interpreter
```

## Phase Dependencies

**Phase 4.0.x (Json, Union Types, Expression Annotations, Error Rename, Struct Type Params, Structural Objects) blocks all later phases.** The type checker requires:
- `Ty::Json` for database values and JSON literals
- `UNION Storable` for `$GET` return type and `$SET` value type
- Union type syntax for function signatures
- Expression type annotations for disambiguation (e.g., `($GET local("key")) : Int`)
- `Error::RuntimeType` distinct from `Error::StaticType`
- Struct types with type parameters (e.g., `TYPE Pair[L, R] = { left: L, right: R }`)
- Structural object types for anonymous records (e.g., `{ name: String, age: Int }`)

Complete 4.0.0, 4.0.0.1, 4.0.1, 4.0.2, 4.0.3, 4.0.4, and 4.0.5 before starting 4.1+.

---

## Phase 4.0.2: Expression Type Annotations [x]

Add support for inline type annotations on expressions. Currently only `LET x: T = e` and function parameters support annotations; we need `(expr) : T` or `expr : T` syntax.

### Syntax

Parentheses required around annotated expressions (like Haskell):

```rumps
; Annotate any expression
LET x = ($GET local("key")) : Int      ; disambiguate DB read
LET y = (1 + 2) : Float               ; force widening
LET z = (Option.None) : Option[String] ; specify type parameter

; Simple literals still need parens
LET a = (42) : Int
```

### Checklist

- [x] Lexer: `:` already exists (used in LET, object literals, function params)
- [x] Parser/CST: Parse `(expr) : type` — annotation only valid after closing paren
- [x] AST: Add `Expr::Annotate { expr: ExprId, ty: AstTypeExprId }`
- [x] Interpreter: Evaluate inner expr, verify type matches annotation (runtime check for now)
- [x] Tests: Expression annotation parsing and evaluation

---

## Phase 4.0.3: Rename Error::Type to Error::RuntimeType [x]

Before implementing the type checker, rename the existing runtime type error to distinguish it from static type errors. This is needed because `Storable AS T` is typed as infallible but may fail at runtime.

### Rationale

The type checker will add `Error::StaticType(TypeError)` for compile-time errors. The existing `Error::Type` is for runtime type mismatches (e.g., `Storable AS Int` when the value is actually a `String`). Renaming avoids confusion.

### Checklist

- [x] Rename `Error::Type` to `Error::RuntimeType` in `error.rs`
- [x] Rename `Error::type_err()` to `Error::runtime_type()`
- [x] Update all call sites (grep for `type_err`, `Error::Type`)
- [x] Update `Diagnostic` impl: change to `"rumps::runtime_type"`
- [x] Verify tests still pass

---

## Phase 4.0.4: Struct Type Parameters [x]

Add parametric polymorphism support for struct types. Currently, sum types and union types support type parameters (e.g., `TYPE Either[L, R] = Left(L) | Right(R)`), but struct types explicitly reject them.

### Syntax

```rumps
; Struct with type parameters
TYPE Pair[L, R] = { left: L, right: R }
TYPE Box[T] = { value: T }
TYPE Node[T] = { data: T, next: Option[Node[T]] }  ; recursive (if supported)

; Usage
LET p: Pair[Int, String] = { left: 42, right: "hello" }
LET b: Box[Array[Int]] = { value: [1, 2, 3] }
```

### Why This Is Needed

1. **Generic containers**: Users may want to define reusable struct types like `Pair[L, R]`
2. **JSON deserialization**: `json READ StructType[T]` needs to work with parameterized structs
3. **Consistency**: Sum types and unions already support type parameters; structs should too

### Checklist

- [x] Update `TypeDef::Struct` in `value.rs` to include `type_params: SmallVec<[StringId; 2]>`
- [x] Remove the rejection check in `interpreter.rs:475-481` that errors on struct type params
- [x] Intern and store type parameters when registering struct types
- [x] Call `validate_type_params()` on all field types (already exists for sum types)
- [x] Update `TypeRegistry` methods that work with struct definitions
- [x] Tests: Struct type parameter parsing and instantiation

---

## Phase 4.0.5: Structural Object Types [x]

Replace the opaque `Object` primitive type with structural anonymous object types. Objects become typed by their fields: `{ name: String, age: Int }` rather than the untyped `Object`.

### Implementation Notes

**Parser Stack Overflow Mitigation**: The `type_pattern()` parser for `IS` expressions must use `boxed()` on the field parser to avoid stack overflow during parser construction. Additionally, `boxed()` is used at strategic points in the expression parser chain (postfix, mul, cmp, read) to reduce stack depth. This is a chumsky-specific issue where deeply nested parser combinators exhaust the stack during construction, not parsing.

**NOTE**: Named struct types declared via `TYPE` are unaffected. This phase adds anonymous structural object types for inline use in annotations, `IS` checks, and function signatures.

### Motivation

Currently, `Object` is a primitive type with no field information:

```rumps
LET obj = { name: "Alice", age: 30 }
$OUTPUT obj IS Object        ; TRUE, but says nothing about fields
```

With structural object types:

```rumps
LET obj = { name: "Alice", age: 30 }
$OUTPUT obj IS { name: String, age: Int }     ; TRUE
$OUTPUT obj IS { name: String }               ; TRUE (extensible-record)
$OUTPUT obj IS { name: Int }                  ; FALSE (wrong field type)
$OUTPUT obj IS { missing: String }            ; FALSE (missing field)
```

### Extensible Record Semantics

Structural object types use extensible-record semantics: an object matches a type if it has _at least_ the required fields with matching types. Extra fields are allowed.

```rumps
LET x = { a: 1, b: 2, c: 3 }

$OUTPUT x IS { a: Int }                  ; TRUE (has field `a: Int`)
$OUTPUT x IS { a: Int, b: Int }          ; TRUE (has both fields)
$OUTPUT x IS { a: Int, b: Int, c: Int }  ; TRUE (exact match)
$OUTPUT x IS { d: Int }                  ; FALSE (missing `d`)
```

This is consistent with how named struct types already work:

```rumps
TYPE Point = { x: Int, y: Int }
LET p = { x: 1, y: 2, z: 3 }  ; extra field `z`
$OUTPUT p IS Point             ; TRUE (has required fields)
```

### Syntax

Structural object types can appear anywhere a type expression is valid:

```rumps
; Type annotations
LET obj: { name: String, age: Int } = { name: "Alice", age: 30 }

; Function parameters and return types
FUN get-name(person: { name: String }) -> String {
  person.name
}

; Function returning structural object
FUN make-point(x: Int, y: Int) -> { x: Int, y: Int } {
  { x: x, y: y }
}

; IS checks
IF val IS { id: Int, data: String } {
  $OUTPUT val.id
}

; In union types
UNION Config = { host: String, port: Int } | { path: String }
```

### Removing `Object` as User-Facing Type

The `Object` type name becomes unavailable to users:

```rumps
; BEFORE (removed)
LET obj: Object = { a: 1 }
$OUTPUT obj IS Object

; AFTER (error)
LET obj: Object = { a: 1 }    ; ERROR: unknown type `Object`
$OUTPUT obj IS Object          ; ERROR: unknown type `Object`

; Use structural types instead
LET obj: { a: Int } = { a: 1 }
$OUTPUT obj IS { a: Int }
```

**Internal note**: `TypeId::OBJECT` and `Value::Object` remain for runtime representation. Only the user-facing type name is removed.

### Object Module Removed

The `Object` module (`Object.keys`, `Object.values`, etc.) has been removed. Dynamic field iteration is incompatible with static typing; objects have heterogeneous field types, so `Object.values` would return `Array[Unknown]` which defeats the purpose of type checking.

For dynamic key-value iteration, use `Map[String, V]` instead of structural objects. Structural objects are for statically-typed records with known field types.

### Implementation Notes

**Parser**: The type expression parser must recognize `{ field: Type, ... }` as a structural object type. This is similar to struct definition bodies but appears in type position.

**Disambiguation**: `{ ... }` in expression position is a value; in type position it's a structural type. Context determines interpretation:
- `LET x: { a: Int } = ...` — type position (after `:`)
- `LET x = { a: 1 }` — expression position

**Named vs Anonymous**: Named structs (`TYPE Foo = { ... }`) create a nominal type registered in `TypeRegistry`. Anonymous structural types (`{ a: Int }`) are not registered; they exist only as `TypeExpr::Object` / `AstTypeExpr::Object`.

### Checklist

#### Lexer (no changes needed)
- [x] `{`, `}`, `:`, `,` tokens already exist

#### Parser / CST
- [x] Add `TypeExprKind::Object(SmallVec<[(String, TypeExpr); 4]>)` variant
- [x] Parse `{ field: Type, ... }` in type expression context
- [x] Distinguish from map type syntax (if any) and expression-level object literals
- [x] Add `cst::TypePattern::Object` for IS expression patterns (uses `boxed()` to avoid stack overflow)

#### AST
- [x] Add `AstTypeExpr::Object(SmallVec<[(String, AstTypeExprId); 4]>)` variant
- [x] Add `TypePattern::Object(SmallVec<[(String, AstTypeExprId); 4]>)` for IS patterns
- [ ] Update `Value::type_name` to render the object with field types
  - E.g. `{ name: String }`, not `Object`
  - **NOTE**: This may require refactor to get the types of the fields
    - I.e. we may need another method or move `Value::type_name`, or pass in type arena to resolve type correctly
    - Or we may need to annotate `Value::Object` with types of fields to render correctly

#### Lowering (CST → AST)
- [x] Lower `cst::TypeExprKind::Object` to `AstTypeExpr::Object`
- [x] Lower `cst::TypePattern::Object` to `TypePattern::Object`

#### TypeExprArena / TypeExpr (value.rs)
- [x] Add `TypeExpr::Object(IndexMap<StringId, TypeExprId>)` variant
- [x] Add `TypeExprArena::object(fields: IndexMap<StringId, TypeExprId>) -> TypeExprId` helper
- [x] Add `TypeExprArena::object_fields()` accessor
- [x] Update `TypeExprArena::format` to display `{ field: Type, ... }`
- [x] Update `TypeExprArena::eq` for **structural** object equality

#### Interpreter: resolve_type_expr (types.rs)
- [x] Handle `AstTypeExpr::Object`: resolve each field type, intern field names, create `TypeExpr::Object`

#### Interpreter: IS checking (types.rs / pattern.rs)
- [x] Update `value_matches_type_expr` to handle `TypeExpr::Object`:
  - Check value is `Value::Object`
  - For each field in type: check object has field with matching type (recursive)
  - Extra fields in object are OK (extensible-record)
- [x] Handle `TypePattern::Object` in `check_pattern` for IS expressions

#### Interpreter: value_type_expr (types.rs)
- [x] Update to return `TypeExpr::Object` with actual field types for `Value::Object`
  - Previously returned `TypeExpr::Named(TypeId::OBJECT)`
  - Now builds structural type from object's actual fields

#### Interpreter: READ conversions (types.rs)
- [x] Support `READ { field: Type, ... }` for JSON to structural object conversion
- [x] Support `READ NamedStruct` for JSON to named struct conversion

#### TypeRegistry: Remove Object registration
- [ ] Remove `Object` from user-accessible type names
  - Option A: Don't register "Object" name in `register_builtins`
  - Option B: Register but mark as internal-only (reject in `resolve_type_expr`)
  - Choose the best/correct option here
- [x] Keep `TypeId::OBJECT` constant for internal use (base type detection)
- [x] Keep `BuiltinType::Object` for `Value::type_name` error messages

#### Static Type Checker (typecheck/ty.rs)
- [x] `Ty::Object(IndexMap<StringId, Ty>)` already exists; no changes needed
- [x] Unification for structural objects is covered in Phase 4.12

#### Tests
- [x] Structural object type parsing: `{ a: Int }`, `{ a: Int, b: Int }`
- [x] Nested structural types: `{ user: { name: String } }`
- [x] IS checks with structural types
- [x] Function params/returns with structural types
- [x] Extensible record semantics (superset matches subset type)
- [x] Object module still works with structural types

#### Migration
- [x] Update `scripts/86_json.rumps` line 47: `$OUTPUT obj IS Object` → `$OUTPUT obj IS { name: String, age: Int }`
- [x] Update any other test scripts using `IS Object`
- [x] Update any other test scripts that `$OUTPUT` and object (`Value::type_name` has changed)

---

## Phase 4.0.0: Add Json Type

Before implementing the type checker, add the `Json` type to rumps-query. This is needed for database values (which can store JSON) and completes the value type system.

### JSON Literal Syntax

Distinguish native objects from JSON objects by key quoting:

```rumps
; Native object (unquoted keys)
{ id: 123, name: "John" }

; JSON object (quoted keys)
{ "id": 123, "name": "John" }
```

### JSON Access Operators

JSON-returning operators (`.`, `->`) return `Json` directly; missing fields return `Json::Null`.
Scalar-extracting operators (`..`, `->>`) return `Option[T]` where `T` is the native RUMPS scalar type.

| Operator   | Description                        | Example          | Result                                   |
|------------|------------------------------------|------------------|------------------------------------------|
| `.field`   | Static field access (returns JSON) | `data.name`      | `Json`                                   |
| `..field`  | Static scalar extraction           | `data..name`     | `Option[Scalar]`                         |
| `->(expr)` | Dynamic key access (returns JSON)  | `data->("name")` | `Json`                                   |
| `->>(expr)`| Dynamic scalar extraction          | `data->>("name")`| `Option[Scalar]`                         |

**NOTE**: The `..field` syntax requires NO space before `..`. With a space before `..`, it becomes the range operator. For example:
- `data..field` → JSON scalar extraction
- `1 .. 10` → Range from 1 to 10

**Scalar extraction rules for `..` and `->>`:**
- JSON `null` or missing field -> `Option.None`
- JSON `true`/`false` -> `Option.Some(Bool)`
- JSON number (integer) -> `Option.Some(Int)`
- JSON number (fractional) -> `Option.Some(Float)`
- JSON string -> `Option.Some(String)`
- JSON object/array -> runtime error (not a scalar; use `READ` instead)

**Usage examples:**
```rumps
LET data = { "name": "John", "age": 30, "active": true }

; JSON field access (returns Json; Json is opaque)
LET name = data.name              ; Json
LET missing = data.foo            ; Json (internally null)

; Static scalar extraction with .. (returns Option with native type)
LET name_str = data..name         ; Option.Some("John") : Option[String]
LET age = data..age               ; Option.Some(30) : Option[Int]
LET active = data..active         ; Option.Some(true) : Option[Bool]
LET missing-val = data..foo       ; Option.None

; Unwrap with !
$OUTPUT data..name!                ; "John"
$OUTPUT data..age! + 1             ; 31 (Int arithmetic works)

; Dynamic access with -> and ->>
LET key = "name"
LET dyn-json = data->(key)        ; Json
LET dyn-scalar = data->>(key)     ; Option[String]

; Coalesce with ??
LET miss = data..missing ?? Option.Some("default")
$OUTPUT miss!                      ; "default"

; Use READ to convert Json to native types (see "JSON is Opaque" section)
LET age-result = data.age READ Int   ; Result.Ok(30)
```

### JSON is Opaque

`Value::Json` is an **opaque** wrapper around `serde_json::Value`. There is no `Json.Null`, `Json.Object`, `Json.Array`, etc. Users cannot pattern match on JSON values directly.

To convert JSON to native RUMPS types, use `READ`:

```rumps
LET data = { "name": "John", "age": 30, "scores": [95, 87, 92] }

; Convert JSON to native types via READ
LET name: Result[String, String] = data.name READ String
LET age: Result[Int, String] = data.age READ Int
LET scores: Result[Array[Int], String] = data.scores READ Array[Int]

; READ with Option[T] handles null gracefully
LET maybe-age = data.age READ Option[Int]     ; Result.Ok(Option.Some(30))
LET maybe-foo = data.foo READ Option[Int]     ; Result.Ok(Option.None) if null or missing

; READ on non-matching types returns Result.Err
LET bad = data.name READ Int                  ; Result.Err("expected Int, got String")

; Convert JSON object to named struct type
TYPE Person = { name: String, age: Int }
LET person = data READ Person                 ; Result[Person, String]

; READ into structural object type works too
LET obj = data READ { name: String, age: Int }  ; Result[{ name: String, age: Int }, String]
```

**READ conversion rules for JSON:**

| JSON Value     | `READ T`              | Result                                        |
|----------------|-----------------------|-----------------------------------------------|
| `null`         | `READ T` (non-Option) | `Result.Err("expected T, got null")`          |
| `null`         | `READ Option[T]`      | `Result.Ok(Option.None)`                      |
| `true`/`false` | `READ Bool`           | `Result.Ok(Bool)`                             |
| number (int)   | `READ Int`            | `Result.Ok(Int)`                              |
| number (float) | `READ Float`          | `Result.Ok(Float)`                            |
| string         | `READ String`         | `Result.Ok(String)`                           |
| array          | `READ Array[T]`       | `Result.Ok(Array[T])` if all elements convert |
| object         | `READ StructType`     | `Result.Ok(StructType)` (named or structural) |

**Note**: JSON objects can be read into structural object types (`{ name: String }`) or named struct types (`TYPE Person = ...`). Both work because they specify the expected fields and their types, enabling validation during READ. The old opaque `Object` type couldn't be used with READ because it had no field information to validate against.

### JSON Arrays

JSON arrays are created in two ways:

**1. Heterogeneous elements (implicit JSON)**
```rumps
[1, 'a', 10.01]           ; Value::Json (heterogeneous = must be JSON)
[true, "hello", 42]       ; Value::Json
```

**2. Explicit cast with `as Json`**
```rumps
[1, 2, 3] as Json         ; Value::Json (homogeneous array cast to JSON)
"hello" as Json           ; Value::Json (string literal)
42 as Json                ; Value::Json (int literal)
3.14 as Json              ; Value::Json (float literal)
true as Json              ; Value::Json (bool literal)
{ a: 1 }                  ; Value::Json (object literal to JSON object)
```

### Checklist

- [x] Lexer: Add `ArrowArrow` (`->>`) token
- [x] Lexer: Add `DotDotNoSpace` token for `..field` (when no space before `..`)
  - Range operator `..` requires spaces: `a .. b`
- [x] Parser/CST: Detect quoted vs unquoted object keys
- [x] Parser/CST: Parse JSON access operators
  - `..field` for static scalar extraction (uses `DotDotNoSpace`)
  - `->(expr)` for dynamic JSON access
  - `->>(expr)` for dynamic scalar extraction
- [x] AST: Add `Expr::Json` variant
- [x] AST: Add `Expr::JsonAccess` with `JsonAccessKind` and `JsonAccessKey`
- [x] Interpreter: Evaluate JSON literals to `Value::Json`
- [x] Interpreter: Implement JSON field access operators
  - `.field` on JSON returns `Json` (null for missing)
  - `..field` returns `Option[Scalar]`
  - `->(expr)` returns `Json`
  - `->>(expr)` returns `Option[Scalar]`
- [x] Value: Add `Value::Json` wrapping `serde_json::Value`
- [x] Tests: JSON literal parsing and access (`scripts/86_json.rumps`)

---

## Phase 4.0.0.1: JSON READ for Struct Types [x]

Extend `READ` to support converting JSON objects to named struct types (including parametric structs). Currently `READ` only handles primitives (`Bool`, `Int`, `Float`, `String`) and structural object types.

### Syntax

```rumps
TYPE Person = { name: String, age: Int }
TYPE Box[T] = { value: T }

LET data = { "name": "Alice", "age": 30 }
LET person = data READ Person              ; Result[Person, String]

LET boxed = { "value": 42 }
LET box = boxed READ Box[Int]              ; Result[Box[Int], String]
```

### Why This Is Needed

1. **Type-safe deserialization**: Users should be able to read JSON into typed structs
2. **Parametric struct support**: `Box[Int]` needs type parameter substitution during READ
3. **Error messages**: Provide clear errors when JSON doesn't match struct schema

### Implementation Notes

- `read_value` in `types.rs` currently matches on `TypeId` for primitives
- For struct types, need to:
  1. Check the value is a JSON object
  2. Look up struct definition (including type params)
  3. For each required field, recursively READ the JSON field to the expected type
  4. Build a `Value::Object` with the converted fields
  5. Return `Result.Err` if any field is missing or has wrong type

### Checklist

- [x] Update `read_value` to handle user-defined struct `TypeId`s
- [x] Add `read_json_to_struct` helper that takes `TypeExprId` (for type args)
- [x] Recursively READ nested fields using resolved field types
- [x] Handle parametric structs by resolving field types with substitution
- [x] Return clear error messages for missing/mismatched fields
- [x] Tests: `json READ StructType`, `json READ StructType[T]`

---

## Phase 4.0.1: Add Union Types

Add genuine union types to the language. This enables typed database operations where `$GET` returns `UNION Storable = Bool | Int | Float | Char | String | Json`.

**NOTE**: `UNION`s must support _all_ RUMPS types, including user-defined `TYPE` declarations. They should also be able to take type parameters, e.g. `UNION F[T] = Int | Option[T]`

### Syntax

```rumps
; Declare a union type
UNION Storable = Bool | Int | Float | Char | String | Json

; Use in annotations
LET x: Storable = $GET local("key")

; Check with IS
IF x IS String {
  $OUTPUT x ++ " is a string"
} ELSE {
  $OUTPUT "not a string"
}

; Cast with AS (runtime, may fail)
; NOTE: This is a special case for `Storable`
LET s: String = x AS String
```

### Union Types in Functions

Union types can be used as both parameter types and return types:

```rumps
; Union as parameter type
FUN stringify(x: Storable) -> String {
  IF x IS String { x }
  ELSE { IF x IS Int { x AS String } ELSE { "unknown" } }
}

; Union as return type
FUN parse(s: String) -> Int | String {
  LET n = s READ Int
  IF n IS Result.Ok(n) { 
    n 
  } ELSE {
    s
  }
}

; Inline union in parameter
FUN double(x: Int | Float) -> Float {
  IF x IS Int { (x * 2) AS Float }
  ELSE { x * 2.0 }
}

; Function returning different union members
FUN fetch-value(key: String) -> Storable {
  LET raw = $GET local(key)
  raw  ; returns Storable (whatever was stored)
}

; Narrowing a union with IS
FUN process(val: Storable) -> String {
  MATCH val {
    x IS Int    => "integer: " ++ (x AS String),
    x IS Float  => "float: " ++ (x AS String),
    x IS String => "string: " ++ x,
    x IS Bool   => "bool: " ++ (x AS String),
    _           => "other"
  }
}

; Closure with union types
LET handler: (Storable) -> Bool = x => x IS String || x IS Int
```

### Built-in Storable Union

Define a built-in union for database-storable values:

```rumps
UNION Storable = Bool | Int | Float | Char | String | Json
```

This is the return type of `$GET` and element type of `COLLECT` (pre-`SELECT`).

### Built-in Scalar Union

Define a built-in union for JSON scalar extraction (used by `->>` operator):

```rumps
UNION Scalar = Bool | Int | Float | String
```

The `->>` operator returns `Option[Scalar]`; the inner type is one of the scalar types that can be extracted from a JSON value. Notes:
- `Char` is intentionally excluded since JSON has no char type
- `Null` is intentionally excluded; the `Option` wrapper handles null (and missing) cases as `Option.None`

### Checklist

- [x] Lexer: Add `UNION` keyword
- [x] Parser/CST: Parse `UNION Name = Type | Type | ...` declarations
  - [x] Handle any parametric polymorphism, e.g. `UNION F[T] = ...`
- [x] Parser/CST: Parse union types in annotations (`x: Int | String`)
- [x] AST: Add `Stmt::Union` for declarations
- [x] AST: Add `AstTypeExpr::Union(SmallVec<[AstTypeExprId; 4]>)` for union type expressions
- [x] TypeRegistry: Register union types
- [x] Value: No change needed (unions are type-level, not value-level)
- [x] Interpreter: `IS` checks against union members
- [x] Interpreter: `AS` casts within union (runtime check)
- [x] Parser/CST: Parse `x IS Type` patterns in MATCH arms
- [x] AST: Add `Pattern::Is { binding, ty }` variant for type-narrowing patterns
- [x] Interpreter: Evaluate `IS` patterns in `MATCH` (runtime type check + binding)
- [x] Builtins: Define `Storable` union
  - [x] **NOTE**: Treat `AS` as infallible _only_ for `Storable` to concrete member types
    - I.e. users can _always_ narrow from `Storable` to concrete type; runtime type error if not successful
    - This is ergonomic choice for making DB access easier
- [x] Tests: Union declaration, IS checks, AS casts

**NOTE**: `$GET` return type is a type-checker concern (no runtime change); see Phase 4.10.

---

## Phase 4.1: Foundation

Create the core type representation and infrastructure.

### File Structure
```
crates/rumps-query/src/intern.rs      -- StringId, StringInterner (shared with interpreter)
crates/rumps-query/src/typecheck.rs   -- pub fn check(ast, registry) -> Result<(), Vec<TypeError>>
crates/rumps-query/src/typecheck/
  ty.rs                               -- Ty, TyVar, Scheme, Subst
  env.rs                              -- TypeEnv (scoped type bindings + StringInterner)
  error.rs                            -- TypeError enum
```

### Type Representation (`ty.rs`)

```rust
// Complete
```

### Checklist

- [x] Create `typecheck.rs` with module declarations
- [x] Create `typecheck/ty.rs`:
  - [x] `TyVar` newtype
  - [x] `Ty` enum with all variants (including `Json`)
  - [x] `impl Ty`:
    - [x] `fn free_vars(&self) -> HashSet<TyVar>`
    - [x] `fn occurs(&self, v: TyVar) -> bool`
    - [x] `fn apply(&self, subst: &Subst) -> Ty`
  - [x] `Scheme` struct
  - [x] `impl Scheme`:
    - [x] `fn mono(ty: Ty) -> Self`
    - [x] `fn instantiate(&self, next: &mut u32) -> Ty` (uses counter instead of InferCtx)
  - [x] `Subst` struct
  - [x] `impl Subst`:
    - [x] `fn empty() -> Self`
    - [x] `fn singleton(v: TyVar, ty: Ty) -> Self`
    - [x] `fn apply(&self, ty: &Ty) -> Ty`
    - [x] `fn compose(&self, other: &Subst) -> Subst`
    - [x] `fn extend(&mut self, v: TyVar, ty: Ty)`
- [x] Create `intern.rs` (shared with interpreter):
  - [x] `StringId` newtype (`u32` index)
  - [x] `StringInterner` struct wrapping `IndexSet<String>`
  - [x] `impl StringInterner`:
    - [x] `fn new() -> Self`
    - [x] `fn intern(&mut self, s: &str) -> StringId`
    - [x] `fn get(&self, id: StringId) -> Option<&str>`
    - [x] `fn lookup(&self, s: &str) -> Option<StringId>`
    - [x] `fn len(&self) -> usize`
- [x] Create `typecheck/env.rs`:
  - [x] `TypeEnv` struct with `scopes: Vec<HashMap<StringId, Scheme>>` and `strings: StringInterner`
  - [x] `impl TypeEnv`:
    - [x] `fn new() -> Self`
    - [x] `fn push_scope(&mut self)`
    - [x] `fn pop_scope(&mut self)`
    - [x] `fn bind(&mut self, name: &str, scheme: Scheme)` (interns name)
    - [x] `fn lookup(&self, name: &str) -> Option<&Scheme>` (looks up via interner)
    - [x] `fn intern(&mut self, s: &str) -> StringId`
    - [x] `fn get_str(&self, id: StringId) -> Option<&str>`
    - [x] `fn free_vars(&self) -> HashSet<TyVar>`
    - [x] `fn generalize(&self, ty: &Ty) -> Scheme`
    - [x] `fn apply(&mut self, subst: &Subst)` (additional helper)
- [x] Create `typecheck/error.rs`:
  - [x] `TypeError` enum (wrapped by `Error::Type` in main error.rs):
    - [x] `Mismatch { expected: Ty, got: Ty, span: Span }`
    - [x] `UndefinedVar(String, Span)`
    - [x] `NotCallable(Ty, Span)`
    - [x] `ArityMismatch { expected: usize, got: usize, span: Span }`
    - [x] `NotNumeric(Ty, Span)`
    - [x] `NotJsonable(Ty, Span)` - for `as Json`, `store`, etc.
    - [x] `NotSubscript(Ty, Span)` - for $SET/$GET subscript keys
    - [x] `NotStorable(Ty, Span)` - for $SET value (must be DB-storable)
    - [x] `MissingField { ty: TypeId, field: String, span: Span }` - struct missing required field
    - [x] `FieldTypeMismatch { ty: TypeId, field: String, expected: Ty, got: Ty, span: Span }`
    - [x] `InfiniteType(TyVar, Ty, Span)`
    - [x] `MissingAnnotation(Span)`
    - [x] `UnknownType(String, Span)`
    - [x] `NonExhaustiveMatch(Span)` - match expression doesn't cover all cases
    - [x] `NotUnwrappable(Ty, Span)` - postfix `!` on non-Option/Result type
  - [x] Derive `Error` via `thiserror`
  - [x] Integrate with `crate::Error` via `Error::Type(TypeError)` (code: `rumps::type`)

---

## Phase 4.1.1: Union Type Representation

Add the `Ty::Union` variant for anonymous union types and wire named unions into the type checker.

### Design

There are two kinds of unions in RUMPS:

| Kind          | Example                                 | Representation              | Semantics                                  |
|---------------|-----------------------------------------|-----------------------------|--------------------------------------------|
| **Named**     | `Storable`, `Scalar`, `UNION Foo = ...` | `Ty::Named(TypeId, params)` | Nominal identity; special runtime behavior |
| **Anonymous** | `Int \| String`                         | `Ty::Union(Vec<Ty>)`        | Structural; "one of these types"           |

Named unions keep their `Ty::Named` representation because:
- `Storable` has special `AS` semantics (infallible cast with runtime error)
- Named unions can have type parameters: `UNION F[T] = Int | Option[T]`
- User-defined unions have registered names for error messages

Anonymous unions become `Ty::Union(vec![...])` for structural matching.

### Helper: `expand_union_members`

To handle unification and `IS`/`AS` checks, we need to expand unions to their members:

```rust
fn expand_union_members(&self, ty: &Ty) -> Option<Vec<Ty>> {
    match ty {
        Ty::Union(members) => Some(members.clone()),
        Ty::Named(id, params) => {
            // Look up in registry; if TypeDef::Union, resolve members
            self.registry.get_def(*id).and_then(|def| match def {
                TypeDef::Union { members, type_params, .. } => {
                    // Build substitution from type_params -> params
                    // Resolve each member TypeExprId to Ty
                    Some(...)
                }
                _ => None,
            })
        }
        _ => None,
    }
}
```

### Checklist

- [x] Add `Ty::Union(Vec<Self>)` to `ty.rs`
- [x] Update `Ty::free_vars()` for `Union` variant
- [x] Update `Ty::occurs()` for `Union` variant
- [x] Update `Ty::apply()` for `Union` variant
- [x] Update `ast_type_to_ty` to handle `AstTypeExpr::Union` → `Ty::Union`
- [x] Add `expand_union_members(ty: &Ty) -> Option<Vec<Ty>>` helper to `InferCtx`
- [x] Unit tests:
  - [x] `Ty::Union` free vars collection
  - [x] `Ty::Union` occurs check
  - [x] `Ty::Union` substitution application
  - [x] `ast_type_to_ty` for anonymous unions
  - [x] `expand_union_members` for named unions (`Storable`, `Scalar`)
  - [x] `expand_union_members` for anonymous unions

---

## Phase 4.2: Inference Context

Create the main inference context and constraint types.

### File Structure
```
crates/rumps-query/src/typecheck/
  infer.rs     -- InferCtx, constraint generation
  constraint.rs -- Constraint enum (optional, can be in infer.rs)
```

### Inference Context (`infer.rs`)

```rust
// Complete
```

### Checklist

- [x] Create `typecheck/infer.rs`:
  - [x] `Constraint` enum
  - [x] `InferCtx` struct
  - [x] `impl InferCtx`:
    - [x] `fn new(ast: &Ast, registry: &TypeRegistry) -> Self`
    - [x] `fn fresh_var(&mut self) -> TyVar`
    - [x] `fn fresh(&mut self) -> Ty` (returns `Ty::Var(self.fresh_var())`)
    - [x] `fn constrain(&mut self, c: Constraint)`
    - [x] `fn unify(&mut self, t1: Ty, t2: Ty, span: Span)` (adds `Eq` constraint)
    - [x] `fn record_type(&mut self, id: ExprId, ty: Ty)`
    - [x] `fn error(&mut self, e: TypeError)`
- [x] Add `mod infer` to `typecheck.rs`

---

## Phase 4.3: Literal and Variable Inference

Infer types for the simplest expressions.

### Inference Rules

| Expression      | Type                             |
|-----------------|----------------------------------|
| `true`, `false` | `Bool`                           |
| `42`            | `Int`                            |
| `3.14`          | `Float`                          |
| `'c'`           | `Char`                           |
| `"hello"`       | `String`                         |
| `Unit`          | `Unit`                           |
| `x` (variable)  | instantiate from `env.lookup(x)` |

### Checklist

- [x] `impl InferCtx`: `fn expr(&mut self, id: ExprId) -> Ty`
- [x] Handle `Expr::Bool` -> `Ty::Bool`
- [x] Handle `Expr::Int` -> `Ty::Int`
- [x] Handle `Expr::Float` -> `Ty::Float`
- [x] Handle `Expr::Char` -> `Ty::Char`
- [x] Handle `Expr::String` -> `Ty::String`
- [x] Handle `Expr::Var`:
  - [x] Look up in `env`
  - [x] If found, instantiate scheme with fresh vars
  - [x] If not found, emit `TypeError::UndefinedVar`

---

## Phase 4.4: Operator Inference

Infer types for unary and binary operators.

### Binary Operator Rules

| Operator                 | Constraint                            | Result Type                                      |
|--------------------------|---------------------------------------|--------------------------------------------------|
| `+`, `-`, `*`, `%`, `**` | `Numeric(lhs)`, `Numeric(rhs)`        | `Float` if either is `Float`, else fresh numeric |
| `/`                      | `Numeric(lhs)`, `Numeric(rhs)`        | `Float`                                          |
| `//`                     | `Numeric(lhs)`, `Numeric(rhs)`        | `Int`                                            |
| `<`, `>`, `<=`, `>=`     | `lhs ~ rhs`                           | `Bool`                                           |
| `==`, `!=`               | `lhs ~ rhs`                           | `Bool`                                           |
| `&&`, `\|\|`             | `lhs ~ Bool`, `rhs ~ Bool`            | `Bool`                                           |
| `++`                     | `lhs ~ String`, `Stringable(rhs)`     | `String` (implicit coercion)                     |
| `??`                     | `lhs ~ Option[?t]` or `Result[?t, _]` | `?t` (unify with `rhs`)                          |
| `\|>`                    | `Callable(rhs, [lhs], ?r)`            | `?r`                                             |
| `..`, `..=`              | `lhs ~ Int`, `rhs ~ Int`              | `Range`                                          |

### Unary Operator Rules

| Operator      | Constraint         | Result Type     |
|---------------|--------------------|-----------------|
| `-`           | `Numeric(operand)` | same as operand |
| `!` (prefix)  | `operand ~ Bool`   | `Bool`          |

### Checklist

- [x] `impl InferCtx`: `fn binary(&mut self, lhs: ExprId, op: BinOp, rhs: ExprId, span: Span) -> Ty`
- [x] Handle arithmetic ops (`Add`, `Sub`, `Mul`, `Mod`, `Pow`):
  - [x] Add `Numeric` constraints for both operands
  - [x] Result: fresh var with numeric constraint (or `Float` if either operand is `Float`)
- [x] Handle `Div` -> always `Float`
- [x] Handle `FloorDiv` -> always `Int`
- [x] Handle comparison ops -> `Bool`
- [x] Handle logical ops (`And`, `Or`) -> unify both with `Bool`, return `Bool`
- [x] Handle `Concat` -> add `Stringable` constraint for both, return `String`
- [x] Handle `Coalesce`:
  - [x] Check lhs is `Option[?t]` or `Result[?t, _]`
  - [x] Unify rhs with `?t`
  - [x] Return `?t`
- [x] Handle `Pipe`:
  - [x] Add `Callable` constraint
  - [x] Return fresh var for result
- [x] Handle `Range`, `RangeInclusive` -> `Range`
- [x] `impl InferCtx`: `fn unary(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> Ty`
- [x] Handle `Neg` -> add `Numeric` constraint, return same type
- [x] Handle `Not` -> unify with `Bool`, return `Bool`

---

## Phase 4.5: Collection Inference [x]

Infer types for arrays, tuples, objects, maps, and ranges.

### Collection Rules

| Expression          | Type               | Constraints                        |
|---------------------|--------------------|------------------------------------|
| `[a, b, c]`         | `Array[?t]`        | `a ~ ?t`, `b ~ ?t`, `c ~ ?t`       |
| `[]`                | `Array[?t]`        | (empty, `?t` is fresh)             |
| `(a, b, c)`         | `(?a, ?b, ?c)`     | -                                  |
| `{ x: a, y: b }`    | `{ x: ?a, y: ?b }` | structural object type             |
| `{ k => v, ... }`   | `Map[?k, ?v]`      | all keys ~ `?k`, all values ~ `?v` |
| `a .. b`, `a ..= b` | `Range`            | `a ~ Int`, `b ~ Int`               |

### Checklist

- [x] Handle `Expr::Array`:
  - [x] If empty, return `Ty::Array(fresh())`
  - [x] Infer first element type `?t`
  - [x] Unify all subsequent elements with `?t`
  - [x] Return `Ty::Array(?t)`
- [x] Handle `Expr::Tuple`:
  - [x] Infer each element
  - [x] Return `Ty::Tuple(vec![...])`
- [x] Handle `Expr::Object`:
  - [x] Infer each field value
  - [x] Return `Ty::Object(IndexMap { field: ty, ... })`
  - [x] **NOTE**: You need to ensure that this works with nested object types!
- [x] Handle `Expr::MapLit`:
  - [x] Infer key and value types
  - [x] Unify all keys, unify all values
  - [x] Return `Ty::Map(key_ty, val_ty)`
- [x] **Note**: Range (` .. `, ` ..= `) is handled in Phase 4.4 (operators) but listed here as it's a collection type

---

## Phase 4.6: Access and Indexing

Infer types for field access, tuple indexing, and array indexing.

### Access Rules

| Expression   | Type                  | Constraints                                                              |
|--------------|-----------------------|--------------------------------------------------------------------------|
| `obj.field`  | `?t`                  | `obj` has field with type `?t`                                           |
|              |                       | Has to work with both anonymous and "struct" objects declared via `TYPE` |
|              |                       |                                                                          |
| `obj.?field` | `Option[?t]`          | optional field access                                                    |
| `tuple.0`    | element type at index | -                                                                        |
| `arr[i]`     | `?t`                  | `arr ~ Array[?t]`, `i ~ Int`                                             |

**NOTE**: The preceding table was written before "Phase 4.0.0: Add Json Type". Refer to that phase for implemented JSON operators and their result type. E.g. `..`, `->>`

### Checklist

- [x] Handle `Expr::Field`:
  - [x] If base is structural object (`Ty::Object`), look up field type
  - [x] If base is type variable, create structural object constraint `{ field: ?t }`
  - [x] If base is `Ty::Named` with `TypeDef::Struct`, look up field in registry
  - [x] If base is `Unknown`, return fresh type variable
  - [x] If field missing, emit `FieldNotFound` error
  - [x] If base is non-object type, emit `NotAnObject` error
- [x] Handle `Expr::OptionalField`:
  - [x] If base is `Option[T]`, extract field from `T` and wrap in `Option`
  - [x] If base is type variable, unify with `Option[?t]` and extract field
  - [x] If base is not `Option`, emit `Mismatch` error
- [x] Handle `Expr::TupleIndex`:
  - [x] Check base is `Tuple`
  - [x] Extract type at index (emit `TupleIndexOutOfBounds` if out of bounds)
  - [x] If base is type variable, return fresh var (cannot infer structure)
  - [x] If base is non-tuple, emit `NotATuple` error
- [x] Handle `Expr::Index`:
  - [x] Check base is `Array[?t]` or `Map[?k, ?v]`
  - [x] For array: unify index with `Int`, return element type
  - [x] For map: unify index with key type, return value type
  - [x] For string: unify index with `Int`, return `Char`
  - [x] If base is non-indexable, emit `NotIndexable` error
- [x] Handle `Expr::JsonAccess`:
  - [x] Base must be `Json` (emit `NotJson` if not)
  - [x] For `JsonAccessKind::Json`, return `Ty::Json`
  - [x] For `JsonAccessKind::Scalar`, return `Option[Scalar]`
  - [x] For dynamic key (`JsonAccessKey::Expr`), unify key with `String`
- [x] Handle `Expr::Json` literals: return `Ty::Json`
- [x] Add unit tests for all access and indexing inference

### Struct Field Type Resolution

For `TYPE` structs, field types are resolved via `ast_type_to_ty()` which converts the `AstTypeExprId` stored in `TypeDef::Struct.fields` to a `Ty`. Type parameter substitution is handled by building a `HashMap<StringId, Ty>` from `type_params` to `type_args`.

---

## Phase 4.7: Function and Closure Inference [x]

Infer types for closures, function calls, and function definitions.

### Function Rules

| Expression            | Type                                |
|-----------------------|-------------------------------------|
| `(x, y) => body`      | `Fn([?x, ?y], ?body)`               |
| `(x: Int) => body`    | `Fn([Int], ?body)`                  |
| `f(a, b)`             | `?r` with `Callable(f, [a, b], ?r)` |

### Checklist

- [x] Handle `Expr::Closure`:
  - [x] For each param: use annotation if present, else fresh var
  - [x] Push scope, bind params
  - [x] Infer body type
  - [x] Pop scope
  - [x] If return annotation present, unify body with it
  - [x] Return `Ty::Fn(param_types, body_type)`
- [x] Handle `Expr::Call`:
  - [x] Infer callee type
  - [x] Infer arg types
  - [x] Create fresh var `?r` for result
  - [x] Add `Callable` constraint
  - [x] Return `?r`
- [x] Handle `Stmt::Fun`:
  - [x] Extract param types (annotations or fresh)
  - [x] Push scope, bind params
  - [x] Infer body
  - [x] Pop scope
  - [x] If return annotation, unify
  - [x] Generalize and bind function name in env

---

## Phase 4.8: Control Flow

Infer types for conditionals, blocks, and match expressions.

### Control Flow Rules

| Expression                | Type        | Constraints                     |
|---------------------------|-------------|---------------------------------|
| `IF c { a } ELSE { b }`   | `?t`        | `c ~ Bool`, `a ~ ?t`, `b ~ ?t`  |
| `{ ... e }` (block)       | type of `e` | -                               |
| `MATCH e { p => b, ... }` | `?t`        | all branches ~ `?t`; exhaustive |

### Branch Type Consistency

All branches of `IF`/`ELSE` and all arms of `MATCH` must evaluate to the same type. This is enforced by unifying all branch types together.

**IF/ELSE examples:**

```rumps
; OK: both branches are Int
LET x = IF cond { 42 } ELSE { 0 }

; OK: both branches are String
LET y = IF cond { "yes" } ELSE { "no" }

; ERROR: branch type mismatch (Int vs String)
LET z = IF cond { 42 } ELSE { "hello" }
;                 ^^           ^^^^^^^
;                 Int          String — cannot unify
```

**Single-arm IF must be Unit:**

```rumps
; OK: body is Unit (statement-like)
IF cond { $OUTPUT "hello" }

; ERROR: body is Int, but no ELSE branch
LET x = IF cond { 42 }
;                 ^^ Int, but ELSE would be Unit — mismatch
```

**MATCH examples:**

**NOTE**: Matches _must_ be exhaustive. Currently, this is done in the interpreter runtime. Exhaustiveness checking should be re-implemented in type-checker (do not remove runtime checks yet).

```rumps
; OK: all arms return Int
LET x = MATCH opt {
  Option.Some(n) => n
  Option.None => 0
}

; ERROR: arm type mismatch (Int vs String)
LET y = MATCH opt {
  Option.Some(n) => n          ; Int
  Option.None => "default"     ; String — cannot unify
}

; OK: all arms return same type after narrowing
LET desc: String = MATCH val {
  v IS Int    => "integer: " ++ v
  v IS String => "string: " ++ v
  v IS Bool   => "bool: " ++ v
  _           => "other"
}
```

### Checklist

- [x] Handle `Expr::If`:
  - [x] Unify condition with `Bool`
  - [x] If condition is `IS` with pattern bindings:
    - [x] Extract bindings and their inferred types
    - [x] Add bindings to then-branch scope (not else-branch)
  - [x] Infer both branches (with appropriate scopes)
  - [x] Unify branch types; emit `TypeError::Mismatch` if incompatible
  - [x] For single-arm IF (no ELSE): unify body with `Unit`
  - [x] Return unified type
- [x] Handle `Expr::Block`:
  - [x] Push scope
  - [x] Infer all statements
  - [x] If final expr, return its type
  - [x] Else return `Unit`
  - [x] Pop scope
- [x] Handle `Expr::Match`:
  - [x] Infer scrutinee type
  - [x] For each arm:
    - [x] Check pattern against scrutinee type
    - [x] Bind pattern variables in arm scope
    - [x] Handle `Pattern::Is` (type-narrowing pattern):
      - [x] Check target type is member of scrutinee's union (if union)
      - [x] Bind variable with narrowed type in arm scope
    - [x] Infer arm body type
  - [x] Unify all arm body types; emit `TypeError::Mismatch` if incompatible
  - [x] **Exhaustiveness check**: verify patterns cover all cases
    - [x] Currently done at runtime in `try_match_arms` (`interpreter/control.rs`); re-implement in type checker **but do not remove** from interpreter yet
    - [x] For sum types: all variants must be covered (or wildcard present)
    - [x] For literals (numbers, strings, chars, etc...): require wildcard/else arm
    - [x] Emit `TypeError::NonExhaustiveMatch` if not exhaustive
  - [x] Return unified type

---

## Phase 4.9: Variant and Sum Type Inference

Infer types for variant constructors and pattern matching.

### Variant Rules

| Expression       | Type                      |
|------------------|---------------------------|
| `Option.None`    | `Option[?t]` (fresh)      |
| `Option.Some(x)` | `Option[typeof(x)]`       |
| `Result.Ok(x)`   | `Result[typeof(x), ?e]`   |
| `Result.Err(e)`  | `Result[?t, typeof(e)]`   |
| User variant     | `Named(TypeId, [params])` |

### Checklist

- [x] Handle `Expr::Variant`:
  - [x] Look up type and variant in registry
  - [x] Infer arg types
  - [x] Match against variant arity
  - [x] For `Option`/`Result`: construct parameterized type
  - [x] For user types: construct `Ty::Named`
- [x] Handle pattern matching on variants:
  - [x] Extract payload types from scrutinee
  - [x] Bind to pattern variables
  
**NOTE**: Almost entirely handled in 4.8

---

## Phase 4.10: Special Expressions [x]

Handle unwrap, IS, AS, READ, $GET, and other special cases.

### Special Rules

| Expression     | Type                | Notes                                                              |
|----------------|---------------------|--------------------------------------------------------------------|
| `e!` (unwrap)  | `?t`                | `Unwrappable(e, ?t)` constraint; works for `Option` and `Result`   |
| `e IS T`       | `Bool`              | runtime check; may introduce bindings (see below)                  |
| `e AS T`       | `T`                 | infallible cast                                                    |
| `e READ T`     | `Result[T, String]` | fallible conversion                                                |
| `$GET local(k)` | `Storable`          | returns `Storable` union; narrow with `IS`/`AS` or usage inference |

#### Note on `IS` with Pattern Bindings

`IS` can be used with destructuring patterns, similar to Rust's `if let`:

```rumps
LET r = Result.Ok(999)
IF r IS Result.Ok(data) {
    $OUTPUT data          ; `data` is bound here with type Int
} ELSE {
    $OUTPUT "error"
}
```

The `IS` expression itself still types as `Bool`. However, when used as an IF condition with bindings:
1. The type checker types `r IS Result.Ok(data)` as `Bool`
2. The then-branch scope gets `data` bound with the extracted payload type (`Int` in this case)
3. The else-branch does NOT have `data` in scope

This is handled in `Expr::If` inference, not in `Expr::Is` — the IF recognizes when its condition is an `IS` with bindings and propagates them to the then-branch.

#### Note on `Storable AS _`

For ergonomics, we should treat _any_ `Storable AS T`, where `T` is a member of the `Storable` union, as infallible. That is, users can _always_ cast from `Storable` to one of those concrete types. We will fall back on a `crate::Error::RuntimeType` error. This does **not** apply to other uses of `AS`, which should be infallible (i.e. `T` to `String`, `Int` to `Float`, etc...)

### Checklist

- [x] Handle `Expr::Unwrap` (postfix `!`):
  - [x] Infer operand type
  - [x] Create fresh var `?t` for inner type
  - [x] Add `Unwrappable { ty: operand_ty, inner: ?t, span }` constraint
  - [x] Return `?t`
- [x] Handle `Expr::Is`:
  - [x] Always returns `Bool`
  - [x] If pattern has bindings, record them for use by enclosing `IF`
  - [x] Infer payload types from the pattern (e.g., `Result.Ok(x)` extracts `x: T` from `Result[T, E]`)
- [x] Handle `Expr::As`:
  - [x] Parse target type from annotation
  - [x] If target is `Json`, add `Jsonable` constraint on operand
  - [x] If operand type is `Storable` and target is a member type, return target (**infallible**)
  - [x] If operand type is `T` and target is `String`, return target (**infallible**; all types coerce to `String`)
  - [x] Otherwise, require type compatibility (e.g., `Int AS Float` for widening)
  - [x] For incompatible types, emit error; e.g. "use `READ` for fallible conversion or `MATCH`/`IS` for narrowing"
- [x] Handle `Expr::Read`:
  - [x] Parse target type
  - [x] Return `Result[T, String]`
- [x] Handle `Expr::Get`:
  - [x] Return `Ty::Named(TypeId::STORABLE, vec![])` (the `Storable` union)
  - [x] Usage may narrow to specific member (e.g., `x + 1` narrows to `Int | Float`)
- [x] Handle `Expr::Annotate`:
  - [x] Infer inner expression type
  - [x] Parse annotation to `Ty`
  - [x] Unify inferred type with annotation (annotation is expected type)
  - [x] Return annotation type

---

## Phase 4.11: Statements

Infer types for all statement types.

### Statement Rules

| Statement          | Effect                                                                        |
|--------------------|-------------------------------------------------------------------------------|
| `LET x = e`        | bind `x` to `typeof(e)` in env                                                |
| `LET x: T = e`     | unify `typeof(e) ~ T`, bind `x` to `T`                                        |
| `$SET local(k) = e` | no env binding (db write)                                                     |
| `$KILL local(k)`    | no env binding (db delete); infer subscripts with `Subscript` constraint      |
| `$OUTPUT e`         | infer `e`, no constraint on type (all types satisfy `Constraint::Stringable`) |
| `expr`             | infer `e` for side effects; no env binding                                    |
| `TYPE T = ...`     | register in type registry                                                     |
| `UNION T = ...`    | register in type registry                                                     |

### Checklist

- [x] `impl InferCtx`: `fn stmt(&mut self, id: StmtId)`
- [x] Handle `Stmt::Let`:
  - [x] Infer RHS type
  - [x] If type annotation present:
    - [x] Parse annotation to `Ty`
    - [x] Unify RHS type with annotation type
    - [x] For `Named` struct annotations: triggers extensible record check
  - [x] Generalize and bind in env
- [x] Handle `Stmt::Set`:
  - [x] Infer subscripts and value
  - [x] Add `Subscript` constraint for each subscript expression
  - [x] Add `Storable` constraint for value (or infer via usage)
  - [x] No env binding
- [x] Handle `Stmt::Kill`:
  - [x] Infer subscripts
  - [x] Add `Subscript` constraint for each subscript expression
  - [x] No env binding
- [x] Handle `Stmt::Output`:
  - [x] Infer expression
  - [x] Add `Stringable` constraint (always satisfied; marks stringify needed)
- [x] Handle `Stmt::Expr`:
  - [x] Infer expression for side effects
  - [x] No env binding; discard result type
- [x] Handle `Stmt::Fun`:
  - [x] (already covered in Phase 4.7)
- [x] Handle `Stmt::Type`:
  - [x] Register type definition in registry
  - [x] For struct types: store required fields and their types
  - [x] No inference needed (declaration only)
- [x] Handle `Stmt::Union`:
  - [x] Register union definition in registry
  - [x] No inference needed (declaration only)

---

## Phase 4.12: Unification Algorithm

Implement constraint solving.

### Unification Rules

```
unify(Var(v), t) = { v -> t } if v not in fv(t)
unify(t, Var(v)) = { v -> t } if v not in fv(t)
unify(Int, Float) = {} (coercion)
unify(Float, Int) = {} (coercion)
unify(Array(a), Array(b)) = unify(a, b)
unify(Fn(p1, r1), Fn(p2, r2)) = unify(p1, p2) . unify(r1, r2)
unify({ f1 }, { f2 }) = unify common fields (structural objects)
unify(Unknown, _) = {}
unify(_, Unknown) = {}
unify(T, T) = {}
unify(_, _) = error
```

### Checklist

- [x] Create `typecheck/unify.rs`
- [x] `impl InferCtx`: `fn unify_types(&mut self, t1: &Ty, t2: &Ty, span: Span) -> Option<Subst>`
- [x] Handle `Var` binding (with occurs check)
- [x] Handle primitive equality
- [x] Handle numeric coercion (`Int` ~ `Float`)
- [x] Handle `Array`, `Option`, `Result`, `Map` recursively
- [x] Handle `Tuple` (element-wise, same length)
- [x] Handle `Fn` (params + return)
- [x] Handle structural objects (`Ty::Object`): unify common fields, allow extras
- [x] Handle `Named` (same TypeId, unify params)
- [x] Handle `Named` struct with structural object (extensible record check):
  - [x] Look up required fields from TypeRegistry
  - [x] Check all required fields present in structural object
  - [x] Unify each required field's type
  - [x] Extra fields in structural object are allowed (extensible)
- [x] Handle `Unknown` (unifies with anything)
- [x] Handle `Error` (unifies with anything, for recovery)
- [x] `impl InferCtx`: `fn solve_constraints(&mut self) -> Subst`
  - [x] Process `Eq` constraints via unification
  - [x] Process `Numeric` constraints (check resolved type is `Int` or `Float`)
  - [x] Process `Callable` constraints (unify with `Fn` type)
  - [x] Process `Stringable` constraints (always satisfied; marks implicit coercion)
  - [x] Process `Jsonable` constraints (reject `Closure`, `Function`, `ModuleFn`)
  - [x] Process `Subscript` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [x] Process `Storable` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [x] Process `Unwrappable` constraints:
    - [x] Check `ty` is `Option[?t]` or `Result[?t, ?e]`
    - [x] Unify `inner` with extracted `?t`
    - [x] Emit `TypeError::NotUnwrappable` if neither
  - [x] Compose all substitutions

---

## Phase 4.13: Builtin Function Types

Register type schemes for all primitive/module functions.

### Design: Colocated Types

Types are registered **inline** with function implementations in `register_builtins`. This makes it impossible to forget a type; you cannot register a function without its `Scheme`.

**Current pattern (runtime only):**
```rust
Module::from_fns(&[
    ("length", Array::length),
    ("map", Array::placeholder),
])
```

**New pattern (runtime + types):**
```rust
Module::from_prims(&[
    PrimDef { name: "length", f: Array::length, ty: Scheme::poly(|t| Fn([Array(t)], Int)) },
    PrimDef { name: "map", f: Array::placeholder, ty: Scheme::poly2(|t, u| Fn([Array(t), Fn([t], u)], Array(u))) },
])
```

The `PrimDef` struct has named fields; compile error if any field is missing.

```rust
pub(crate) struct PrimDef {
    pub(crate) name: &'static str,
    pub(crate) f: PrimFn,
    pub(crate) ty: Scheme,
}
```

### `Scheme` Construction Helpers

Add ergonomic constructors for common patterns:

```rust
impl Scheme {
    /// Monomorphic type (no type variables).
    fn mono(ty: Ty) -> Self { Scheme { vars: vec![], ty } }

    /// Polymorphic with 1 type variable: `forall T. ...`
    fn poly(f: impl FnOnce(Ty) -> Ty) -> Self {
        let t = Ty::Var(TyVar(0));
        Scheme { vars: vec![TyVar(0)], ty: f(t) }
    }

    /// Polymorphic with 2 type variables: `forall T U. ...`
    fn poly2(f: impl FnOnce(Ty, Ty) -> Ty) -> Self { ... }

    /// Polymorphic with 3 type variables: `forall T U V. ...`
    fn poly3(f: impl FnOnce(Ty, Ty, Ty) -> Ty) -> Self { ... }
}

impl Ty {
    /// Helper: `Fn([A, B], R)` for function types.
    fn func(params: impl Into<Vec<Ty>>, ret: Ty) -> Self {
        Ty::Fn(params.into(), Box::new(ret))
    }
}
```

### Example Signatures

```rust
// Array module
PrimDef { name: "length", f: Array::length, ty: Scheme::poly(|t| Ty::func([Ty::Array(Box::new(t))], Ty::Int)) },
PrimDef { name: "map", f: Array::placeholder, ty: Scheme::poly2(|t, u| {
    Ty::func([Ty::Array(Box::new(t.clone())), Ty::func([t], u.clone())], Ty::Array(Box::new(u)))
})},

// Math module (monomorphic)
PrimDef { name: "abs", f: Math::abs, ty: Scheme::mono(Ty::func([Ty::Float], Ty::Float)) },
PrimDef { name: "sqrt", f: Math::sqrt, ty: Scheme::mono(Ty::func([Ty::Float], Ty::Float)) },
PrimDef { name: "floor", f: Math::floor, ty: Scheme::mono(Ty::func([Ty::Float], Ty::Int)) },
```

**Note**: The `Object` module has been removed. Dynamic field iteration is incompatible with static typing (objects have heterogeneous field types). Use `Map[String, V]` for dynamic key-value collections.

**Note**: Array functions accept `Range` as first arg (Range is iterable over `Int`). Use union type `Array[T] | Range` for first param where applicable.

### Module Changes

Extend `Module` to store type schemes:

```rust
/// A primitive function with its type.
pub(crate) struct PrimDef {
    pub(crate) name: &'static str,
    pub(crate) f: PrimFn,
    pub(crate) ty: Scheme,
}

pub(crate) struct Module {
    fns: HashMap<String, PrimFn>,
    types: HashMap<String, Scheme>,  // NEW
    consts: HashMap<String, ValueId>,
    submodules: HashMap<String, Module>,
}

impl Module {
    /// Register primitives (function + type together).
    pub(crate) fn from_prims(prims: &[PrimDef]) -> Self { ... }

    /// Look up a function's type scheme.
    pub(crate) fn get_fn_type(&self, path: &[&str]) -> Option<&Scheme> { ... }
}
```

The type checker queries `env.get_module_fn_type(&["Array", "length"])` to get the scheme.

### Checklist

- [x] Add `PrimDef` struct to `env.rs`
- [x] Add `Scheme::mono`, `Scheme::poly`, `Scheme::poly2`, `Scheme::poly3` helpers
- [x] Add `Ty::func` helper for function type construction
- [x] Add `types: HashMap<String, Scheme>` field to `Module`
- [x] Add `Module::from_prims(&[PrimDef]) -> Self`
- [x] Add `Module::get_fn_type(&self, path: &[&str]) -> Option<&Scheme>`
- [x] Add `Environment::get_module_fn_type(&self, path: &[&str]) -> Option<&Scheme>`
- [x] Update `register_builtins` to use `PrimDef` for all functions, with correct type scheme:
  - [x] `Array` module (including HoF placeholders)
  - [x] `String` module
  - [x] `Math` module (including `Trig` submodule)
  - [x] `Map` module
  - [x] `Option` module
  - [x] `Result` module
  - [x] `Time` module
  - [x] `Random` module
- [x] Test: missing field in `PrimDef` causes compile error
