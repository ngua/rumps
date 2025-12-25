# Phase 4: Static Type System

Add a Hindley-Milner style type inference and checking phase. The type checker runs after name resolution but before interpretation, rejecting programs with type errors at compile time.

## Design Decisions

| Decision         | Choice                                   | Rationale                                                                               |
|------------------|------------------------------------------|-----------------------------------------------------------------------------------------|
| DB operations    | Infer from usage, fallback to `Storable` | `LET x = GET local(1); x + 1` infers `Int`; ambiguous cases resolve to `Storable` union |
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
- `UNION Storable` for `GET` return type and `SET` value type
- Union type syntax for function signatures
- Expression type annotations for disambiguation (e.g., `(GET local("key")) : Int`)
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
LET x = (GET local("key")) : Int      ; disambiguate DB read
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
OUTPUT obj IS Object        ; TRUE, but says nothing about fields
```

With structural object types:

```rumps
LET obj = { name: "Alice", age: 30 }
OUTPUT obj IS { name: String, age: Int }     ; TRUE
OUTPUT obj IS { name: String }               ; TRUE (extensible-record)
OUTPUT obj IS { name: Int }                  ; FALSE (wrong field type)
OUTPUT obj IS { missing: String }            ; FALSE (missing field)
```

### Extensible Record Semantics

Structural object types use extensible-record semantics: an object matches a type if it has _at least_ the required fields with matching types. Extra fields are allowed.

```rumps
LET x = { a: 1, b: 2, c: 3 }

OUTPUT x IS { a: Int }                  ; TRUE (has field `a: Int`)
OUTPUT x IS { a: Int, b: Int }          ; TRUE (has both fields)
OUTPUT x IS { a: Int, b: Int, c: Int }  ; TRUE (exact match)
OUTPUT x IS { d: Int }                  ; FALSE (missing `d`)
```

This is consistent with how named struct types already work:

```rumps
TYPE Point = { x: Int, y: Int }
LET p = { x: 1, y: 2, z: 3 }  ; extra field `z`
OUTPUT p IS Point             ; TRUE (has required fields)
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
  OUTPUT val.id
}

; In union types
UNION Config = { host: String, port: Int } | { path: String }
```

### Removing `Object` as User-Facing Type

The `Object` type name becomes unavailable to users:

```rumps
; BEFORE (removed)
LET obj: Object = { a: 1 }
OUTPUT obj IS Object

; AFTER (error)
LET obj: Object = { a: 1 }    ; ERROR: unknown type `Object`
OUTPUT obj IS Object          ; ERROR: unknown type `Object`

; Use structural types instead
LET obj: { a: Int } = { a: 1 }
OUTPUT obj IS { a: Int }
```

**Internal note**: `TypeId::OBJECT` and `Value::Object` remain for runtime representation. Only the user-facing type name is removed.

### Object Module Unchanged

The `Object` module (`Object.keys`, `Object.values`, etc.) remains available. These functions operate on any structural object type:

```rumps
LET obj: { a: Int, b: String } = { a: 1, b: "hello" }
OUTPUT Object.keys(obj)       ; ["a", "b"]
OUTPUT Object.values(obj)     ; [1, "hello"]
```

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
- [x] Update `scripts/86_json.rumps` line 47: `OUTPUT obj IS Object` → `OUTPUT obj IS { name: String, age: Int }`
- [x] Update any other test scripts using `IS Object`
- [x] Update any other test scripts that `OUTPUT` and object (`Value::type_name` has changed)

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
OUTPUT data..name!                ; "John"
OUTPUT data..age! + 1             ; 31 (Int arithmetic works)

; Dynamic access with -> and ->>
LET key = "name"
LET dyn-json = data->(key)        ; Json
LET dyn-scalar = data->>(key)     ; Option[String]

; Coalesce with ??
LET miss = data..missing ?? Option.Some("default")
OUTPUT miss!                      ; "default"

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

Add genuine union types to the language. This enables typed database operations where `GET` returns `UNION Storable = Bool | Int | Float | Char | String | Json`.

**NOTE**: `UNION`s must support _all_ RUMPS types, including user-defined `TYPE` declarations. They should also be able to take type parameters, e.g. `UNION F[T] = Int | Option[T]`

### Syntax

```rumps
; Declare a union type
UNION Storable = Bool | Int | Float | Char | String | Json

; Use in annotations
LET x: Storable = GET local("key")

; Check with IS
IF x IS String {
  OUTPUT x ++ " is a string"
} ELSE {
  OUTPUT "not a string"
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
  LET raw = GET local(key)
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

This is the return type of `GET` and element type of `COLLECT` (pre-`SELECT`).

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

**NOTE**: `GET` return type is a type-checker concern (no runtime change); see Phase 4.10.

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
    - [x] `NotSubscript(Ty, Span)` - for SET/GET subscript keys
    - [x] `NotStorable(Ty, Span)` - for SET value (must be DB-storable)
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
struct InferCtx<'a> {
    ast: &'a Ast,
    registry: &'a TypeRegistry,
    env: TypeEnv,
    constraints: Vec<Constraint>,
    next_var: u32,
    expr_types: HashMap<ExprId, Ty>,
    errors: Vec<TypeError>,
}

enum Constraint {
    Eq(Ty, Ty, Span),
    Numeric(Ty, Span),                                         // Int | Float
    Callable { callee: Ty, args: Vec<Ty>, ret: Ty, span: Span },
    Stringable(Ty, Span),                                      // everything (for ++ coercion, OUTPUT)
    Jsonable(Ty, Span),                                        // NOT Closure/Function/ModuleFn
    Subscript(Ty, Span),                                       // Bool | Int | Float | Char | String | Json
    Storable(Ty, Span),                                        // Bool | Int | Float | Char | String | Json
    Unwrappable { ty: Ty, inner: Ty, span: Span },             // Option[?t] | Result[?t, _]; extracts ?t
}
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
IF cond { OUTPUT "hello" }

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

- [ ] Handle `Expr::If`:
  - [ ] Unify condition with `Bool`
  - [ ] If condition is `IS` with pattern bindings:
    - [ ] Extract bindings and their inferred types
    - [ ] Add bindings to then-branch scope (not else-branch)
  - [ ] Infer both branches (with appropriate scopes)
  - [ ] Unify branch types; emit `TypeError::Mismatch` if incompatible
  - [ ] For single-arm IF (no ELSE): unify body with `Unit`
  - [ ] Return unified type
- [ ] Handle `Expr::Block`:
  - [ ] Push scope
  - [ ] Infer all statements
  - [ ] If final expr, return its type
  - [ ] Else return `Unit`
  - [ ] Pop scope
- [ ] Handle `Expr::Match`:
  - [ ] Infer scrutinee type
  - [ ] For each arm:
    - [ ] Check pattern against scrutinee type
    - [ ] Bind pattern variables in arm scope
    - [ ] Handle `Pattern::Is` (type-narrowing pattern):
      - [ ] Check target type is member of scrutinee's union (if union)
      - [ ] Bind variable with narrowed type in arm scope
    - [ ] Infer arm body type
  - [ ] Unify all arm body types; emit `TypeError::Mismatch` if incompatible
  - [ ] **Exhaustiveness check**: verify patterns cover all cases
    - [ ] Currently done at runtime in `try_match_arms` (`interpreter/control.rs`); re-implement in type checker **but do not remove** from interpreter yet
    - [ ] For sum types: all variants must be covered (or wildcard present)
    - [ ] For literals (numbers, strings, chars, etc...): require wildcard/else arm
    - [ ] Emit `TypeError::NonExhaustiveMatch` if not exhaustive
  - [ ] Return unified type

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

- [ ] Handle `Expr::Variant`:
  - [ ] Look up type and variant in registry
  - [ ] Infer arg types
  - [ ] Match against variant arity
  - [ ] For `Option`/`Result`: construct parameterized type
  - [ ] For user types: construct `Ty::Named`
- [ ] Handle pattern matching on variants:
  - [ ] Extract payload types from scrutinee
  - [ ] Bind to pattern variables

---

## Phase 4.10: Special Expressions

Handle unwrap, IS, AS, READ, GET, and other special cases.

### Special Rules

| Expression     | Type                | Notes                                                              |
|----------------|---------------------|--------------------------------------------------------------------|
| `e!` (unwrap)  | `?t`                | `Unwrappable(e, ?t)` constraint; works for `Option` and `Result`   |
| `e IS T`       | `Bool`              | runtime check; may introduce bindings (see below)                  |
| `e AS T`       | `T`                 | infallible cast                                                    |
| `e READ T`     | `Result[T, String]` | fallible conversion                                                |
| `GET local(k)` | `Storable`          | returns `Storable` union; narrow with `IS`/`AS` or usage inference |

#### Note on `IS` with Pattern Bindings

`IS` can be used with destructuring patterns, similar to Rust's `if let`:

```rumps
LET r = Result.Ok(999)
IF r IS Result.Ok(data) {
    OUTPUT data          ; `data` is bound here with type Int
} ELSE {
    OUTPUT "error"
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

- [ ] Handle `Expr::Unwrap` (postfix `!`):
  - [ ] Infer operand type
  - [ ] Create fresh var `?t` for inner type
  - [ ] Add `Unwrappable { ty: operand_ty, inner: ?t, span }` constraint
  - [ ] Return `?t`
- [ ] Handle `Expr::Is`:
  - [ ] Always returns `Bool`
  - [ ] If pattern has bindings, record them for use by enclosing `IF`
  - [ ] Infer payload types from the pattern (e.g., `Result.Ok(x)` extracts `x: T` from `Result[T, E]`)
- [ ] Handle `Expr::As`:
  - [ ] Parse target type from annotation
  - [ ] If target is `Json`, add `Jsonable` constraint on operand
  - [ ] If operand type is `Storable` and target is a member type, return target (infallible)
  - [ ] Otherwise, require type compatibility (e.g., `Int AS Float` for widening; any `T` is `Stringable`, etc...)
  - [ ] For incompatible types, emit error; use `READ` for fallible conversion or `MATCH`/`IS` for narrowing
- [ ] Handle `Expr::Read`:
  - [ ] Parse target type
  - [ ] Return `Result[T, String]`
- [ ] Handle `Expr::Get`:
  - [ ] Return `Ty::Named(TypeId::STORABLE, vec![])` (the `Storable` union)
  - [ ] Usage may narrow to specific member (e.g., `x + 1` narrows to `Int | Float`)
- [ ] Handle `Expr::Annotate`:
  - [ ] Infer inner expression type
  - [ ] Parse annotation to `Ty`
  - [ ] Unify inferred type with annotation (annotation is expected type)
  - [ ] Return annotation type

---

## Phase 4.11: Statements

Infer types for all statement types.

### Statement Rules

| Statement          | Effect                                                                        |
|--------------------|-------------------------------------------------------------------------------|
| `LET x = e`        | bind `x` to `typeof(e)` in env                                                |
| `LET x: T = e`     | unify `typeof(e) ~ T`, bind `x` to `T`                                        |
| `SET local(k) = e` | no env binding (db write)                                                     |
| `OUTPUT e`         | infer `e`, no constraint on type (all types satisfy `Constraint::Stringable`) |
| `TYPE T = ...`     | register in type registry                                                     |

### Checklist

- [ ] `impl InferCtx`: `fn stmt(&mut self, id: StmtId)`
- [ ] Handle `Stmt::Let`:
  - [ ] Infer RHS type
  - [ ] If type annotation present:
    - [ ] Parse annotation to `Ty`
    - [ ] Unify RHS type with annotation type
    - [ ] For `Named` struct annotations: triggers extensible record check
  - [ ] Generalize and bind in env
- [ ] Handle `Stmt::Set`:
  - [ ] Infer subscripts and value
  - [ ] Add `Subscript` constraint for each subscript expression
  - [ ] Add `Storable` constraint for value (or infer via usage)
  - [ ] No env binding
- [ ] Handle `Stmt::Output`:
  - [ ] Infer expression
  - [ ] Add `Stringable` constraint (always satisfied; marks stringify needed)
- [ ] Handle `Stmt::Fun`:
  - [ ] (already covered in Phase 4.7)
- [ ] Handle `Stmt::Type`:
  - [ ] Register type definition in registry
  - [ ] For struct types: store required fields and their types
  - [ ] No inference needed (declaration only)

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

- [ ] Create `typecheck/unify.rs`
- [ ] `impl InferCtx`: `fn unify_types(&mut self, t1: &Ty, t2: &Ty, span: Span) -> Option<Subst>`
- [ ] Handle `Var` binding (with occurs check)
- [ ] Handle primitive equality
- [ ] Handle numeric coercion (`Int` ~ `Float`)
- [ ] Handle `Array`, `Option`, `Result`, `Map` recursively
- [ ] Handle `Tuple` (element-wise, same length)
- [ ] Handle `Fn` (params + return)
- [ ] Handle structural objects (`Ty::Object`): unify common fields, allow extras
- [ ] Handle `Named` (same TypeId, unify params)
- [ ] Handle `Named` struct with structural object (extensible record check):
  - [ ] Look up required fields from TypeRegistry
  - [ ] Check all required fields present in structural object
  - [ ] Unify each required field's type
  - [ ] Extra fields in structural object are allowed (extensible)
- [ ] Handle `Unknown` (unifies with anything)
- [ ] Handle `Error` (unifies with anything, for recovery)
- [ ] `impl InferCtx`: `fn solve_constraints(&mut self) -> Subst`
  - [ ] Process `Eq` constraints via unification
  - [ ] Process `Numeric` constraints (check resolved type is `Int` or `Float`)
  - [ ] Process `Callable` constraints (unify with `Fn` type)
  - [ ] Process `Stringable` constraints (always satisfied; marks implicit coercion)
  - [ ] Process `Jsonable` constraints (reject `Closure`, `Function`, `ModuleFn`)
  - [ ] Process `Subscript` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [ ] Process `Storable` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [ ] Process `Unwrappable` constraints:
    - [ ] Check `ty` is `Option[?t]` or `Result[?t, ?e]`
    - [ ] Unify `inner` with extracted `?t`
    - [ ] Emit `TypeError::NotUnwrappable` if neither
  - [ ] Compose all substitutions

---

## Phase 4.13: Builtin Function Types

Register type schemes for all primitive/module functions.

### Example Signatures

```
; Array module (also accepts Range where Array[Int] expected)
Array.len:    forall a. (Array[a] | Range) -> Int
Array.map:    forall a b. (Array[a] | Range, (a) -> b) -> Array[b]
Array.filter: forall a. (Array[a] | Range, (a) -> Bool) -> Array[a]
Array.fold:   forall a b. (Array[a] | Range, b, (b, a) -> b) -> b

; Option module
Option.map:      forall a b. (Option[a], (a) -> b) -> Option[b]
Option.unwrap:   forall a. (Option[a]) -> a
Option.unwrap-or: forall a. (Option[a], a) -> a

; Object module (accepts any structural object via `{ }`)
; `{ }` means "any object with any fields" (empty structural type = wildcard)
Object.keys:    ({ }) -> Array[String]
Object.values:  ({ }) -> Array[Unknown]
Object.entries: ({ }) -> Array[(String, Unknown)]
Object.has:     ({ }, String) -> Bool
Object.lookup:  ({ }, String) -> Option[Unknown]

; Math module
Math.abs:   (Float) -> Float
Math.sqrt:  (Float) -> Float
Math.floor: (Float) -> Int
```

### Checklist

- [ ] Create `typecheck/builtins.rs`
- [ ] `impl InferCtx`: `fn register_builtins(&mut self)`
- [ ] Register `Array` module functions
- [ ] Register `Option` module functions
- [ ] Register `Result` module functions
- [ ] Register `Object` module functions
- [ ] Register `String` module functions
- [ ] Register `Math` module functions
- [ ] Register `Time` module functions
- [ ] Register `Map` module functions
- [ ] Register `Range` module functions (if any)
- [ ] **Note**: Array functions accept `Range` as first arg (Range is iterable over `Int`)

---

## Phase 4.14: Integration and Entry Point

Hook type checking into the interpreter pipeline.

### Entry Point (`typecheck.rs`)

```rust
pub(crate) fn check(ast: &Ast, registry: &TypeRegistry) -> crate::Result<()> {
    let mut ctx = InferCtx::new(ast, registry);
    ctx.register_builtins();

    ast.stmt_ids().for_each(|id| ctx.stmt(id));

    let subst = ctx.solve_constraints();
    ctx.apply_subst(&subst);
    ctx.check_remaining_unknowns();

    ctx.into_result()
}

impl InferCtx<'_> {
    fn into_result(self) -> crate::Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(Error::static_types(self.errors))
        }
    }
}
```

### Mapping Static Types to Runtime Types

The type checker uses `Ty` (with type variables, inference constructs). The interpreter uses `TypeExprId` (runtime type tags). We need a mapping function to bridge these:

```rust
impl TypeExprArena {
    /// Convert a resolved static type to a runtime type expression.
    ///
    /// Panics if `ty` contains unresolved type variables (`Var`, `Unknown`, `Error`).
    pub(crate) fn from_ty(&mut self, ty: &Ty, registry: &TypeRegistry) -> TypeExprId {
        match ty {
            Ty::Bool => self.named(TypeId::BOOL),
            Ty::Int => self.named(TypeId::INT),
            Ty::Float => self.named(TypeId::FLOAT),
            Ty::Char => self.named(TypeId::CHAR),
            Ty::String => self.named(TypeId::STRING),
            Ty::Unit => self.named(TypeId::UNIT),
            Ty::Time => self.named(TypeId::TIME),
            Ty::Range => self.named(TypeId::RANGE),
            Ty::Array(elem) => {
                let elem_id = self.from_ty(elem, registry);
                self.app(TypeId::ARRAY, smallvec![elem_id])
            }
            Ty::Option(inner) => {
                let inner_id = self.from_ty(inner, registry);
                self.app(TypeId::OPTION, smallvec![inner_id])
            }
            Ty::Result(ok, err) => {
                let ok_id = self.from_ty(ok, registry);
                let err_id = self.from_ty(err, registry);
                self.app(TypeId::RESULT, smallvec![ok_id, err_id])
            }
            Ty::Map(k, v) => {
                let k_id = self.from_ty(k, registry);
                let v_id = self.from_ty(v, registry);
                self.app(TypeId::MAP, smallvec![k_id, v_id])
            }
            Ty::Tuple(elems) => {
                let elem_ids: SmallVec<[_; 4]> = elems
                    .iter()
                    .map(|e| self.from_ty(e, registry))
                    .collect();
                self.app(TypeId::TUPLE, elem_ids)
            }
            Ty::Named(type_id, params) => {
                let param_ids: SmallVec<[_; 4]> = params
                    .iter()
                    .map(|p| self.from_ty(p, registry))
                    .collect();
                self.app(*type_id, param_ids)
            }
            Ty::Fn(_, _) => {
                // Functions don't have TypeId representations
                self.named(TypeId::UNKNOWN)
            }
            Ty::Object(fields) => {
                // Convert structural object to TypeExpr::Object
                let converted: IndexMap<StringId, TypeExprId> = fields
                    .iter()
                    .map(|(k, ty)| (*k, self.from_ty(ty, registry)))
                    .collect();
                self.object(converted)
            }
            Ty::Var(_) | Ty::Unknown | Ty::Error => {
                unreachable!("from_ty called on unresolved type: {:?}", ty)
            }
        }
    }
}
```

This is used for:
- Runtime `IS` checks (compare value's type tag against user's annotation)
- Runtime `AS` casts (verify cast is valid)
- Error messages with concrete types

### Checklist

- [ ] Handle `Stmt::Union` in `stmt` (register type, no env binding needed)
- [ ] Resolve user-defined union members in `expand_union_members` (requires `TypeExprArena`)
- [ ] Add `pub(crate) fn check(ast, registry) -> crate::Result<()>` to `typecheck.rs`
- [ ] `impl InferCtx`: `fn into_result(self) -> crate::Result<()>`
- [ ] `impl TypeExprArena`: `fn from_ty(&mut self, ty: &Ty, registry: &TypeRegistry) -> TypeExprId`
- [ ] Modify `crates/rumps-query/src/lib.rs`:
  - [ ] Add `mod typecheck;`
- [ ] Modify `crates/rumps-query/src/interpreter.rs`:
  - [ ] Call `crate::typecheck::check(ast, &registry)?` after resolution
- [ ] Modify `crates/rumps-query/src/error.rs`:
  - [ ] Add `Error::StaticType(TypeError)` variant for compile-time type errors
  - [ ] Add `Error::static_types(Vec<TypeError>) -> Self` constructor:
    - [ ] If single error, return `Error::StaticType(err)`
    - [ ] If multiple, wrap in `Error::Multiple`
  - [ ] Update `Diagnostic` impl: add `"rumps::static_type"` code for new variant
  - [ ] Update `ErrorDisplay` impl for new variant
  - [ ] Update all call sites of `Error::type_err()` to `Error::runtime_type()`

---

## Phase 4.15: Error Messages and Diagnostics

Improve error messages with suggestions and context.

### Checklist

- [ ] Add span information to all error types
- [ ] Implement `Display` for `Ty` (pretty-print types)
- [ ] Add "expected X, got Y" format for mismatches
- [ ] Add suggestions for common mistakes:
  - [ ] "Did you mean to annotate this with `: T`?"
  - [ ] "Cannot use `+` on String; use `++` for concatenation"
  - [ ] "Array elements must have the same type"
- [ ] Integrate with `miette` for nice error rendering

---

## Phase 4.16: Testing

Comprehensive test suite for the type checker.

### Test Categories

1. **Literals and variables**: all primitive types infer correctly
2. **Operators**: numeric, comparison, logical, string
3. **Collections**: array homogeneity, tuple, object, map
4. **Functions**: closures, calls, higher-order
5. **Control flow**: if/else, match, blocks
6. **Variants**: Option, Result, user-defined
7. **Errors**: type mismatches, undefined vars, arity
8. **Inference**: polymorphic functions, generalization
9. **Database**: GET with annotation, usage inference

### Checklist

- [ ] Create `crates/rumps-query/src/typecheck/tests.rs`
- [ ] Add unit tests for each `Ty` method
- [ ] Add unit tests for `Subst` operations
- [ ] Add unit tests for `TypeEnv` scoping
- [ ] Add integration tests for expression inference
- [ ] Add integration tests for statement inference
- [ ] Add integration tests for unification
- [ ] Add error case tests
- [ ] Add polymorphism tests
- [ ] Add end-to-end tests with `.rumps` scripts
  - [ ] Fix existing `.rumps` scripts (many will break)

---

## Critical Files to Modify

1. `crates/rumps-query/src/lib.rs` - add `mod typecheck`
2. `crates/rumps-query/src/interpreter.rs` - call type checker
3. `crates/rumps-query/src/error.rs` - add `TypeError` variant or integrate

## Critical Files to Read Before Implementation

1. `crates/rumps-query/src/ast.rs` - AST structure for traversal
2. `crates/rumps-query/src/value.rs` - `TypeId`, `TypeRegistry` for user types
3. `crates/rumps-query/src/intern.rs` - `StringId`, `StringInterner` for string interning
4. `crates/rumps-query/src/resolve.rs` - resolution pass pattern to follow
5. `crates/rumps-query/src/interpreter/types.rs` - runtime type helpers for reference
6. `crates/rumps-query/src/primitives.rs` - builtin function signatures to type

---

## Phase 4.17: Post-Type-Checker Cleanup

Once the type checker is working and we're confident in its soundness, eliminate redundant runtime type checks throughout the interpreter. This is a significant cleanup affecting ~180+ check sites.

### Summary of Eliminable Checks

| Category                    | Count | Primary Files               |
|-----------------------------|-------|-----------------------------|
| Arity checks                | 50+   | `primitives.rs`             |
| Array homogeneity           | 4     | `collections.rs`, `call.rs` |
| Binary operator type checks | 11+   | `ops.rs`                    |
| Type coercion helpers       | 5+    | `types.rs`, `primitives.rs` |
| IF/Unit checks              | 2     | `control.rs`                |
| Unwrap/Option/Result        | 5+    | `control.rs`                |
| Module function type checks | 100+  | `primitives.rs`             |

---

### The `typechecked!` Macro

Instead of scattering `unreachable!("type checker guarantees ...")` throughout the codebase, define a declarative macro for consistent messaging:

```rust
/// Marks a branch as unreachable due to static type checking.
///
/// Use instead of `unreachable!` when the type checker guarantees a constraint.
/// Provides consistent error messages if the "impossible" case is somehow reached.
macro_rules! typechecked {
    ($op:expr, $constraint:expr) => {
        unreachable!(
            "type checker guarantees `{}` satisfies `{}`",
            $op,
            $constraint
        )
    };
}
```

**Usage examples:**

| Call | Expands to |
|------|------------|
| `typechecked!("+", "Numeric")` | `unreachable!("type checker guarantees \`+\` satisfies \`Numeric\`")` |
| `typechecked!("!", "Unwrappable")` | `unreachable!("type checker guarantees \`!\` satisfies \`Unwrappable\`")` |
| `typechecked!("Array.map", "Array")` | `unreachable!("type checker guarantees \`Array.map\` satisfies \`Array\`")` |
| `typechecked!("..", "Int")` | `unreachable!("type checker guarantees \`..\` satisfies \`Int\`")` |

**Benefits:**
- Consistent error messages across the codebase
- Easy to grep for all type-checker-guaranteed branches
- Single point of change if we want to modify the message format
- Self-documenting: the macro name makes intent clear

**Placement:** Define in `crates/rumps-query/src/interpreter/mod.rs` or a shared `macros.rs` module.

---

### 4.17.1: Remove Arity Checks (`primitives.rs`)

**Current pattern:**
```rust
Self::check_arity("Object.keys", &args, 1, ctx.span)?;
```

**After cleanup:**
```rust
// Arity validated by Callable constraint at compile-time; no check needed
```

**Checklist:**
- [ ] Remove `check_arity` function entirely
- [ ] Remove all `check_arity` calls (~50+ sites across all module functions)
- [ ] Object module functions (`Object.keys`, `Object.values`, etc.)
- [ ] Array module functions (`Array.length`, `Array.map`, etc.)
- [ ] String module functions (`String.length`, `String.split`, etc.)
- [ ] Math module functions (`Math.abs`, `Math.floor`, etc.)
- [ ] Map module functions (`Map.length`, `Map.keys`, etc.)
- [ ] Time module functions (`Time.now`, `Time.parse`, etc.)
- [ ] Random module functions (`Random.int`, `Random.choice`, etc.)
- [ ] Option/Result module functions (`Option.unwrap-or`, `Result.map`, etc.)

---

### 4.17.2: Remove Array Homogeneity Checks (`collections.rs`)

**Current pattern in `array_elems()`:**
```rust
if self.type_exprs.eq(elem_ty, val_ty) {
    // ok
} else {
    Err(Error::type_err(span, "array elements must have the same type"))
}
```

**After cleanup:**
```rust
// Type checker ensures Array[T] elements are all T; no runtime check
```

**Checklist:**
- [ ] `array_elems()`: Remove type equality check
- [ ] `map_lit_entries()`: Remove key type homogeneity check
- [ ] `map_lit_entries()`: Remove value type homogeneity check
- [ ] Remove `TypeExprArena` tracking from `Value::Array` if no longer needed

---

### 4.17.3: Remove Array.map/filter Homogeneity Checks (`call.rs`)

**Current pattern in `array_map_rec()`:**
```rust
if fty == result_ty {
    // ok
} else {
    Err(Error::type_err(span,
        "Array.map: function produces heterogeneous results"))
}
```

**After cleanup:**
```rust
// Type checker infers Array.map : (Array[A], (A) -> B) -> Array[B]
// Result homogeneity is guaranteed statically
```

**Checklist:**
- [ ] `array_map_rec()`: Remove result type homogeneity check
- [ ] `range_map_rec()`: Remove result type homogeneity check
- [ ] `array_filter_rec()`: Remove similar checks if present
- [ ] `array_reduce()`: Verify no type checks needed

---

### 4.17.4: Remove Binary Operator Type Checks (`ops.rs`)

**Current pattern:**
```rust
fn binop_add(&mut self, lhs: ValueId, rhs: ValueId, span: Span) -> Result<Value> {
    match (self.arena.get(lhs), self.arena.get(rhs)) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        // ... more cases ...
        _ => Err(Error::type_err(span, "cannot add these types"))
    }
}
```

**After cleanup:**
```rust
fn binop_add(&mut self, lhs: ValueId, rhs: ValueId) -> Value {
    match (self.arena.get(lhs), self.arena.get(rhs)) {
        (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
        (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
        (Value::Int(a), Value::Float(b)) => Value::Float(*a as f64 + b.0),
        (Value::Float(a), Value::Int(b)) => Value::Float(a.0 + *b as f64),
        _ => typechecked!("+", "Numeric")
    }
}
```

**Checklist:**
- [ ] `apply_unop()`: Remove error branches for `-` and `!`
- [ ] `binop_add()`: Remove error branch
- [ ] `binop_sub()`: Remove error branch
- [ ] `binop_mul()`: Remove error branch
- [ ] `binop_div()`: Remove error branch
- [ ] `binop_floor_div()`: Remove error branch
- [ ] `binop_mod()`: Remove error branch
- [ ] `binop_pow()`: Remove error branch
- [ ] `binop_cmp()`: Remove error branch for comparison operators
- [ ] Change return types from `Result<Value>` to `Value` where possible

---

### 4.17.5: Remove IF/Unit Checks (`control.rs`)

**Current pattern in `check_unit()`:**
```rust
fn check_unit(&self, val: ValueId, span: Span) -> Result<()> {
    let ty = self.type_of(val);
    if self.type_exprs.eq(ty, unit_ty) {
        Ok(())
    } else {
        Err(Error::type_err(span, "single-arm IF body must be Unit"))
    }
}
```

**After cleanup:**
```rust
// Type checker enforces: IF without ELSE must have body : Unit
// No runtime check needed
```

**Checklist:**
- [ ] Remove `check_unit()` function entirely
- [ ] `r#if()`: Remove `check_unit` call
- [ ] `if_with_bindings()`: Remove `check_unit` call
- [ ] `r#if()`: Remove `Bool` check on condition (type checker guarantees `Bool`)
- [ ] `try_match_arms()`: Remove `Bool` check on guard (type checker guarantees `Bool`)
- [ ] `array_filter_rec()`: Remove `Bool` check on predicate result (type checker guarantees `Bool`)
- [ ] `range_filter_rec()`: Remove `Bool` check on predicate result (type checker guarantees `Bool`)

---

### 4.17.6: Remove Unwrap/Coalesce Type Checks (`control.rs`)

**Current pattern in `unwrap()`:**
```rust
let is_option = |ty| base_type(ty) == Some(TypeId::OPTION);
let is_result = |ty| base_type(ty) == Some(TypeId::RESULT);

match val {
    Value::Tagged(ty, idx, _) if is_option(ty) => { ... }
    Value::Tagged(ty, idx, _) if is_result(ty) => { ... }
    _ => Err(Error::type_err(span, "can only unwrap Option or Result"))
}
```

**After cleanup:**
```rust
let Value::Tagged(_, idx, payloads) = val else {
    typechecked!("!", "Unwrappable")
};
```

**Checklist:**
- [ ] `unwrap()`: Remove `is_option`/`is_result` helper closures
- [ ] `unwrap()`: Remove type error branch
- [ ] `coalesce()`: Remove similar type checking logic
- [ ] Simplify to direct pattern matching on `Value::Tagged`

---

### 4.17.7: Remove Range Type Checks (`control.rs`)

**Current pattern in `range()`:**
```rust
let start = match self.arena.get(start_id) {
    Value::Int(n) => *n,
    _ => Err(Error::type_err(span, "range start must be Int"))?
};
```

**After cleanup:**
```rust
let Value::Int(start) = self.arena.get(start_id) else {
    typechecked!("..", "Int")
};
```

**Checklist:**
- [ ] `range()`: Remove start type check
- [ ] `range()`: Remove end type check

---

### 4.17.8: Remove Type Coercion Error Branches (`primitives.rs`, `types.rs`)

**Current pattern in `to_float()`:**
```rust
fn to_float(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(f.0),
        _ => Err(Error::type_err(span, "expected numeric"))
    }
}
```

**After cleanup:**
```rust
fn to_float(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => f.0,
        _ => typechecked!("to_float", "Numeric")
    }
}
```

**Checklist:**
- [ ] `to_float()`: Remove error branch, change return to `f64`
- [ ] `to_int()`: Remove error branch, change return to `i64`
- [ ] Update all call sites of `to_float`/`to_int` to remove `?`

---

### 4.17.9: Remove Module Function Type Checks (`primitives.rs`)

**Current pattern (100+ occurrences):**
```rust
let arr = ctx.arena.get_array(args[0])
    .ok_or_else(|| ctx.type_error("Array.length", "Array"))?;
```

**After cleanup:**
```rust
let Value::Array(_, elems) = ctx.arena.get(args[0]).unwrap() else {
    typechecked!("Array.length", "Array")
};
```

**Checklist by module:**

**Object module:**
- [ ] `Object.keys`: Remove Object type check
- [ ] `Object.values`: Remove Object type check
- [ ] `Object.entries`: Remove Object type check
- [ ] `Object.has`: Remove Object type check
- [ ] `Object.lookup`: Remove Object type check
- [ ] `Object.insert`: Remove Object type check
- [ ] `Object.remove`: Remove Object type check

**Array module:**
- [ ] `Array.length`: Remove Array type check
- [ ] `Array.head`: Remove Array type check
- [ ] `Array.tail`: Remove Array type check
- [ ] `Array.last`: Remove Array type check
- [ ] `Array.init`: Remove Array type check
- [ ] `Array.nth`: Remove Array type check
- [ ] `Array.reverse`: Remove Array type check
- [ ] `Array.concat`: Remove Array type checks
- [ ] `Array.contains`: Remove Array type check
- [ ] `Array.map`: Remove Array/closure type checks
- [ ] `Array.filter`: Remove Array/closure type checks
- [ ] `Array.reduce`: Remove Array/closure type checks
- [ ] `Array.find`: Remove type checks
- [ ] `Array.any`: Remove type checks
- [ ] `Array.all`: Remove type checks
- [ ] `Array.sort`: Remove type checks
- [ ] `Array.sort-by`: Remove type checks

**String module:**
- [ ] `String.length`: Remove String type check
- [ ] `String.chars`: Remove String type check
- [ ] `String.split`: Remove String type checks
- [ ] `String.join`: Remove Array/String type checks
- [ ] `String.trim`: Remove String type check
- [ ] `String.starts-with`: Remove String type checks
- [ ] `String.ends-with`: Remove String type checks
- [ ] `String.contains`: Remove String type checks
- [ ] `String.replace`: Remove String type checks
- [ ] `String.to-upper`: Remove String type check
- [ ] `String.to-lower`: Remove String type check
- [ ] `String.pad-left`: Remove type checks
- [ ] `String.pad-right`: Remove type checks

**Math module:**
- [ ] All Math functions: Remove Float type checks
- [ ] `Math.abs`, `Math.floor`, `Math.ceil`, `Math.round`, etc.

**Map module:**
- [ ] `Map.length`: Remove Map type check
- [ ] `Map.keys`: Remove Map type check
- [ ] `Map.values`: Remove Map type check
- [ ] `Map.entries`: Remove Map type check
- [ ] `Map.has`: Remove Map type check
- [ ] `Map.lookup`: Remove Map type check
- [ ] `Map.insert`: Remove Map type check
- [ ] `Map.remove`: Remove Map type check
- [ ] `Map.merge`: Remove Map type checks
- [ ] `Map.from-entries`: Remove Array type check

**Time module:**
- [ ] `Time.now`: No type checks needed
- [ ] `Time.parse`: Remove String type check
- [ ] `Time.format`: Remove Time/String type checks
- [ ] `Time.add-*`: Remove Time/Int type checks
- [ ] `Time.diff-*`: Remove Time type checks

**Random module:**
- [ ] `Random.int`: Remove Int type checks
- [ ] `Random.float`: Remove Float type checks
- [ ] `Random.choice`: Remove Array type check
- [ ] `Random.shuffle`: Remove Array type check

**Option/Result module:**
- [ ] `Option.unwrap-or`: Remove Option type check
- [ ] `Result.unwrap-or`: Remove Result type check
- [ ] `Option.map`: Remove type checks
- [ ] `Result.map`: Remove type checks
- [ ] `Result.map-err`: Remove type checks

---

### 4.17.10: Simplify Pattern Matching (`pattern.rs`)

**Checklist:**
- [ ] `check_variant_zero_arity()`: Remove runtime arity validation
- [ ] `check_variant()`: Simplify type/variant matching
- [ ] Pattern exhaustiveness is checked statically; remove runtime fallbacks

---

### 4.17.11: Simplify Type Coercion (`types.rs`)

**Checklist:**
- [ ] `coerce()`: Remove unsupported cast error branch
  - Keep: `AS` on `Storable` union remains fallible at runtime
- [ ] `try_convert()`: Remove unsupported conversion error branch
  - Keep: `READ` remains fallible at runtime
- [ ] `value_matches_type()`: May be removable if only used for runtime `IS` checks

---

### Notes

**Keep runtime checks for:**
- `AS` casts on `Storable` union (user may cast `Storable` to wrong concrete type)
- `READ` conversions (parsing can fail)
- Database operations returning `Storable` (need `IS`/`AS` for narrowing)

---

### 4.17.12: Test-Driven Removal Strategy

For each category of runtime checks, follow this process:

**Step 1: Write a failing test**
```rumps
; scripts/err_array_heterogeneous.rumps
; This should be caught by the type checker
LET arr: Array[Int] = [1, "two", 3]  ; ERROR: array elements must have same type
```

**Step 2: Create expected snapshot**
Via `cargo insta`; `miette` will produce a nice error, this is just for example

```
; snapshots/scripts__err_array_heterogeneous.snap
error: type mismatch
  --> err_array_heterogeneous.rumps:2:15
   |
 2 | LET arr: Array[Int] = [1, "two", 3]
   |                           ^^^^^ expected Int, got String
```

**Step 3: Run test**
- If type-checker catches it (test passes with static error): go immediately to Step 4 below
- If type-checker misses it (runtime error or no error): fix type-checker, repeat; then go to Step 4 once fixed

**Step 4: Remove runtime check**
Once the type-checker reliably catches the error, remove the corresponding runtime check code.

---

**Checklist for each check category:**

**Arity checks:**
- [ ] Write test: `Array.map(arr)` (missing second arg)
- [ ] Verify type-checker catches `ArityMismatch`
- [ ] Remove `check_arity` calls

**Array homogeneity:**
- [ ] Write test: `[1, "two", 3]`
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove `array_elems` type equality check

**Binary operators:**
- [ ] Write test: `"hello" + 5`
- [ ] Verify type-checker catches `NotNumeric` or `Mismatch`
- [ ] Remove operator error branches

**IF/Unit:**
- [ ] Write test: `LET x = IF TRUE { 42 }` (no ELSE, body not Unit)
- [ ] Verify type-checker catches the constraint
- [ ] Remove `check_unit` function

**Unwrap:**
- [ ] Write test: `42!` (unwrap on non-Option/Result)
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove unwrap type checks

**Range:**
- [ ] Write test: `"a" .. "z"` (non-Int range bounds)
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove range type checks

**Module functions:**
- [ ] Write test for each module: e.g., `Array.length("not an array")`
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove `.ok_or_else(|| ctx.type_error(...))` patterns

---

**Expected benefits:**
- Smaller binary size (less error handling code)
- Faster execution (no redundant checks)
- Cleaner code (pattern matches without error branches)
- Confidence: each removed check has a test proving the type-checker catches it

---

## Future Considerations: Database Primitives

The following primitives are not yet implemented but will require special type handling when added:

### `DATA`

Returns information about whether a node exists and has data/descendants. Returns a `DataStatus` type (not `Int`):

```rumps
DATA ^global(subscripts...)  ; -> DataStatus
DATA local(subscripts...)    ; -> DataStatus
```

The `DataStatus` type corresponds to `rumps_types::DataStatus`:

```rust
enum DataStatus {
    NoData = 0,        ; no value, no descendants
    HasValue = 1,      ; has value only
    HasDescendants = 10, ; has descendants only
    Both = 11,         ; has both value and descendants
}
```

**Casting to Int**: `DATA ... AS Int` always succeeds and evaluates to the `u8` representation:

```rumps
LET status = DATA ^PATIENT(123)
IF status IS DataStatus.HasValue { ... }

; Or get the numeric value
LET code: Int = DATA ^PATIENT(123) AS Int  ; 0, 1, 10, or 11
```

### `ORDER`

Returns the next/previous subscript key in sorted order. Type depends on subscript type:

```
ORDER(^global(subscripts...), direction) -> Option[Subscript]
```

Where `Subscript` is a union of valid subscript types (`Bool | Int | Float | Char | String | Json`). May need a dedicated `Subscript` type or return `Unknown` and require annotation.

### `COLLECT` (most complex)

This is the declarative iteration primitive:

```
COLLECT ^global(prefix...)
  WHERE predicate
  SELECT transform
  TAKE n
  -> ???
```

Key challenges:
- **Element type inference**: What type does each iteration yield?
- **Predicate typing**: The `WHERE` clause receives bound variables; what are their types?
- **Transform typing**: The `SELECT` clause transforms elements; output type depends on transform
- **Composability**: `COLLECT` is lazy/streaming; type must reflect this (e.g., `Stream[T]` or `Iterator[T]`)

### Approach: Union Types for Database Values

Database types are restricted (see `rumps_types`). We model this with a built-in union:

```rumps
UNION Storable = Bool | Int | Float | Char | String | Json
```

**`Subscript`** (key components) is similar but uses `Number` (f64) internally instead of separate `Int`/`Float`:
```rumps
UNION Subscript = Bool | Number | Char | String | Json
```

For typing:
- `GET local(k)` -> returns `Storable`
- `GET local(k)` used as `x + 1` -> narrow to `Int | Float`
- `GET local(k)` used as `x ++ y` -> narrow to `String`
- `GET local(k)` with no constraining usage -> remains `Storable`
- `ORDER` result -> `Option[Subscript]`
- `COLLECT` element values -> `Storable` (narrowed by `SELECT` usage)

Users can:
1. Let inference narrow the union from usage
2. Use `IS` to check the runtime type
3. Use `AS` to cast (runtime, may fail)

This is more precise than `Any` since we know exactly what the DB can store, and users have first-class tools (`IS`, `AS`) to work with unions.

---

## Note: Truthiness Removed

As part of preparing for the static type system, the concept of "truthiness" was removed from RUMPS. Previously, values like `0`, `""`, `[]`, `{}`, `Option.None`, and `Result.Err` were "falsy", while other values were "truthy". This allowed using any value in `IF` conditions.

With the move to static typing:
- `IF` conditions now require `Bool` type explicitly
- `MATCH` guards require `Bool` type explicitly
- `Array.filter` predicates must return `Bool`

To check if an `Option` has a value, use `opt IS Option.Some(_)` instead of relying on truthiness. This is more explicit and type-safe.

**Removed:**
- `Value::is_truthy()` method
- Truthiness tests in `value.rs`
- Range truthiness tests (`IF 1..5 { ... }`)
- Tuple truthiness tests (`IF pair { ... }`)
- Option truthiness in conditions (`IF result { ... }` → `IF result IS Option.Some(_) { ... }`)
