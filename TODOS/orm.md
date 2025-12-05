# ORM-Like Traits for RUMPS

This document explores the design of `ToRumps`/`FromRumps` traits that would allow automatic conversion between Rust types and RUMPS's hierarchical key-value storage format. Similar to how serde works for serialization, but tailored to RUMPS's tree structure.

## Motivation

For users coming from SQL databases, it would be convenient to work with Rust structs directly rather than manually constructing `Key`s and `Value`s:

```rust
// Instead of this:
let key = Key::from(vec![123.into(), "name".into()]);
txn.set(&Name::Global("patient".into()), &key, Value::String("Alice".into())).await?;

// Write this:
txn.insert(&Patient { id: 123, name: "Alice".into(), age: 30 }).await?;
```

---

## Core Design Questions

### What roles can a field play?

In RUMPS's hierarchical model, a field can be:

1. **A subscript** (part of the key path): `^person(id, ...)`
2. **A value** (terminal): `^person(..., "name") = "Alice"`
3. **A subtree** (nested struct/collection): expands into more key-value pairs

---

## Trait Hierarchy

Three layers of traits, each serving a different purpose:

### Layer 1: Subscript Conversion

Types that can be a single subscript in a key path.

```rust
/// Convert to a subscript (part of a key)
pub trait ToSubscript {
    fn to_subscript(&self) -> Subscript;
}

/// Parse from a subscript
pub trait FromSubscript: Sized {
    fn from_subscript(s: &Subscript) -> Result<Self, DecodeError>;
}
```

Implementations for primitives:
- `u8`, `u16`, `u32`, `u64`, `i8`, `i16`, `i32`, `i64` → `Subscript::Integer`
- `f32`, `f64` → `Subscript::Float`
- `bool` → `Subscript::Boolean`
- `String`, `&str` → `Subscript::String`

### Layer 2: Value Conversion

Types that can be stored as a terminal value.

```rust
/// Convert to a RUMPS value
pub trait ToValue {
    fn to_value(&self) -> Value;
}

/// Parse from a RUMPS value
pub trait FromValue: Sized {
    fn from_value(v: &Value) -> Result<Self, DecodeError>;
}
```

Similar implementations for primitives, plus:
- `Vec<u8>`, `&[u8]` → `Value::Bytes`
- Unit enums (see below)

### Layer 3: Tree Conversion

Types that expand into a tree of key-value pairs (structs, complex enums, collections).

```rust
/// Convert a type into RUMPS key-value pairs
pub trait ToRumps {
    /// The global name this type is stored under (e.g., `"patient"` for `^patient`)
    const GLOBAL: &'static str;

    /// Expand into key-value pairs, prefixed by `prefix`
    fn to_rumps(&self, prefix: &Key) -> Vec<(Key, Value)>;

    /// Extract the key portion (for lookups/deletes)
    fn to_key(&self) -> Key;
}

/// Reconstruct a type from RUMPS key-value pairs
pub trait FromRumps: Sized {
    /// The global name to query
    const GLOBAL: &'static str;

    /// Reconstruct from an iterator of matching key-value pairs
    fn from_rumps<I>(prefix: &Key, pairs: I) -> Result<Self, DecodeError>
    where
        I: Iterator<Item = (Key, Value)>;

    /// What key pattern to query (for the DB to know what to fetch)
    fn key_pattern(prefix: &Key) -> KeyPattern;
}
```

---

## Derive Macro Design

### Struct Attributes

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]  // Required: names the global (^patient)
struct Patient {
    #[rumps(key, order = 0)]  // Part of key, first subscript
    id: u64,

    #[rumps(key, order = 1)]  // Second subscript (for composite keys)
    dept: String,

    name: String,             // Stored as value at ^patient(id, dept, "name")
    age: u32,                 // Stored as value at ^patient(id, dept, "age")

    #[rumps(flatten)]         // Inline nested struct's fields at this level
    address: Address,

    #[rumps(subtree)]         // Store as nested subtree (default for structs)
    emergency_contact: Contact,

    #[rumps(rename = "dob")]  // Custom subscript name
    date_of_birth: u64,

    #[rumps(skip)]            // Don't persist this field
    cached_value: Option<String>,
}
```

### Field Attributes Summary

| Attribute | Effect |
|-----------|--------|
| `#[rumps(key)]` | Field is part of the key path, not stored as value |
| `#[rumps(key, order = N)]` | Explicit ordering for composite keys |
| `#[rumps(flatten)]` | Inline nested struct fields at current level |
| `#[rumps(subtree)]` | Store nested struct as subtree (default for structs) |
| `#[rumps(rename = "x")]` | Use custom name for the subscript |
| `#[rumps(skip)]` | Don't persist this field |

---

## Storage Layouts

### Flat (default for primitives)

```
^patient(123, "cardio", "name") = "Alice"
^patient(123, "cardio", "age") = 30
^patient(123, "cardio", "dob") = 631152000
```

### Subtree (default for nested structs)

```rust
struct Patient {
    #[rumps(key)]
    id: u64,
    emergency_contact: Contact,  // Implicitly #[rumps(subtree)]
}

struct Contact {
    name: String,
    phone: String,
}
```

Produces:
```
^patient(123, "emergency_contact", "name") = "Bob"
^patient(123, "emergency_contact", "phone") = "555-1234"
```

### Flatten (inlines nested struct fields)

```rust
struct Patient {
    #[rumps(key)]
    id: u64,
    #[rumps(flatten)]
    address: Address,
}

struct Address {
    street: String,
    city: String,
}
```

Produces:
```
^patient(123, "street") = "123 Main St"
^patient(123, "city") = "Boston"
```

---

## Enum Handling

Several options depending on the enum's structure:

### Option A: Unit Enums as Values

Unit-only enums can implement `ToValue`/`FromValue` directly:

```rust
#[derive(ToValue, FromValue)]
enum Priority { Low, Medium, High }

struct Task {
    #[rumps(key)]
    id: u64,
    priority: Priority,  // Stored as ^task(id, "priority") = "High"
}
```

### Option B: Tagged Representation (like serde's adjacently tagged)

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(tag = "type")]  // Store discriminant in "type" subscript
enum Status {
    Active,
    Inactive { reason: String },
    Transferred { to_dept: String, date: u64 },
}
```

Produces:
```
^...(key, "status", "type") = "Transferred"
^...(key, "status", "to_dept") = "oncology"
^...(key, "status", "date") = 1234567890
```

### Option C: Discriminant as Subscript

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(enum_key)]  // Use variant name as part of key path
enum Status {
    Active,
    Inactive { reason: String },
    Transferred { to_dept: String, date: u64 },
}
```

Produces:
```
^...(key, "status", "Transferred", "to_dept") = "oncology"
^...(key, "status", "Transferred", "date") = 1234567890
```

### Option D: Externally Tagged (serde default)

```rust
#[derive(ToRumps, FromRumps)]
enum Status {
    Active,
    Inactive { reason: String },
}
```

Produces:
```
^...(key, "status", "Inactive", "reason") = "retired"
```

---

## Collection Handling

### Vec<T>

Use index as subscript:

```rust
struct Patient {
    #[rumps(key)]
    id: u64,
    visits: Vec<Visit>,
}
```

Produces:
```
^patient(123, "visits", 0, "date") = ...
^patient(123, "visits", 0, "notes") = ...
^patient(123, "visits", 1, "date") = ...
^patient(123, "visits", 1, "notes") = ...
```

### HashMap<K, V>

Use map key as subscript (requires `K: ToSubscript`):

```rust
struct Patient {
    #[rumps(key)]
    id: u64,
    labs: HashMap<String, LabResult>,
}
```

Produces:
```
^patient(123, "labs", "CBC", "value") = ...
^patient(123, "labs", "CBC", "date") = ...
^patient(123, "labs", "BMP", "value") = ...
```

### BTreeMap<K, V>

Same as `HashMap`, but iteration order is guaranteed.

### Option<T>

- `Some(v)` → store normally
- `None` → omit the key entirely (don't store anything)

On read, missing keys become `None`.

---

## Database Extension Traits

Sealed extension traits provide ergonomic methods on `Database` and `Transaction`:

### Read Operations (available on both)

```rust
mod private {
    pub trait Sealed {}
    impl Sealed for crate::Database {}
    impl Sealed for crate::Transaction<'_> {}
}

/// Read operations for types implementing `FromRumps`
#[async_trait]
pub trait DatabaseReadExt: private::Sealed {
    /// Get a single record by key
    async fn get<T: FromRumps>(&self, key: impl IntoKey) -> Result<Option<T>, Error>;

    /// Check if a record exists
    async fn exists<T: FromRumps>(&self, key: impl IntoKey) -> Result<bool, Error>;

    /// Iterate all records of type `T`
    async fn all<T: FromRumps>(&self) -> Result<Vec<T>, Error>;

    /// Query records matching a key prefix
    async fn query<T: FromRumps>(
        &self,
        prefix: impl IntoKey
    ) -> Result<Vec<T>, Error>;

    /// Count records matching a prefix
    async fn count<T: FromRumps>(&self, prefix: impl IntoKey) -> Result<usize, Error>;
}

impl DatabaseReadExt for Database { /* ... */ }
impl DatabaseReadExt for Transaction<'_> { /* ... */ }
```

### Write Operations (Transaction only)

This enforces at compile time that writes go through transactions:

```rust
/// Write operations for types implementing `ToRumps`
#[async_trait]
pub trait TransactionWriteExt: private::Sealed {
    /// Insert a new record
    async fn insert<T: ToRumps>(&mut self, val: &T) -> Result<(), Error>;

    /// Delete a record by key
    async fn delete<T: ToRumps>(&mut self, key: impl IntoKey) -> Result<(), Error>;

    /// Update a record (read-modify-write)
    async fn update<T, F>(&mut self, key: impl IntoKey, f: F) -> Result<(), Error>
    where
        T: ToRumps + FromRumps,
        F: FnOnce(&mut T);

    /// Insert or update
    async fn upsert<T: ToRumps>(&mut self, val: &T) -> Result<(), Error>;
}

impl TransactionWriteExt for Transaction<'_> { /* ... */ }
// NOT implemented for Database - writes require transactions!
```

---

## The `IntoKey` Helper Trait

For ergonomic key construction from tuples:

```rust
pub trait IntoKey {
    fn into_key(self) -> Key;
}

// Single values
impl<A: ToSubscript> IntoKey for A {
    fn into_key(self) -> Key {
        Key::from(vec![self.to_subscript()])
    }
}

// Tuples of various sizes
impl<A: ToSubscript> IntoKey for (A,) {
    fn into_key(self) -> Key {
        Key::from(vec![self.0.to_subscript()])
    }
}

impl<A: ToSubscript, B: ToSubscript> IntoKey for (A, B) {
    fn into_key(self) -> Key {
        Key::from(vec![self.0.to_subscript(), self.1.to_subscript()])
    }
}

impl<A: ToSubscript, B: ToSubscript, C: ToSubscript> IntoKey for (A, B, C) {
    fn into_key(self) -> Key {
        Key::from(vec![
            self.0.to_subscript(),
            self.1.to_subscript(),
            self.2.to_subscript(),
        ])
    }
}

// ... continue for reasonable tuple sizes (up to 8 or so)
```

This enables:

```rust
db.get::<Patient>(123).await?;                    // Single key
db.get::<Patient>((123, "cardio")).await?;        // Composite key
db.query::<Patient>(("cardio",)).await?;          // Prefix query
```

---

## Usage Examples

### Basic CRUD

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key)]
    id: u64,
    name: String,
    age: u32,
}

// Create
db.transaction(|txn| async {
    txn.insert(&Patient {
        id: 123,
        name: "Alice".into(),
        age: 30
    }).await?;
    Ok(())
}).await?;

// Read
let patient: Option<Patient> = db.get(123).await?;

// Update
db.transaction(|txn| async {
    txn.update::<Patient, _>(123, |p| {
        p.age = 31;
    }).await?;
    Ok(())
}).await?;

// Delete
db.transaction(|txn| async {
    txn.delete::<Patient>(123).await?;
    Ok(())
}).await?;

// List all
let patients: Vec<Patient> = db.all::<Patient>().await?;
```

### Composite Keys

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key, order = 0)]
    dept: String,
    #[rumps(key, order = 1)]
    id: u64,
    name: String,
}

// Get specific patient
let p: Option<Patient> = db.get(("cardio", 123)).await?;

// Query all patients in cardiology
let cardio_patients: Vec<Patient> = db.query::<Patient>(("cardio",)).await?;
```

### Nested Structures

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key)]
    id: u64,
    name: String,

    #[rumps(flatten)]
    address: Address,

    contacts: Vec<Contact>,
    labs: HashMap<String, LabResult>,
}

#[derive(ToRumps, FromRumps)]
struct Address {
    street: String,
    city: String,
    zip: String,
}

#[derive(ToRumps, FromRumps)]
struct Contact {
    name: String,
    phone: String,
    relationship: String,
}

#[derive(ToRumps, FromRumps)]
struct LabResult {
    value: f64,
    unit: String,
    date: u64,
}
```

Produces storage layout:
```
^patient(123, "name") = "Alice"
^patient(123, "street") = "123 Main St"      // flattened from address
^patient(123, "city") = "Boston"
^patient(123, "zip") = "02101"
^patient(123, "contacts", 0, "name") = "Bob"
^patient(123, "contacts", 0, "phone") = "555-1234"
^patient(123, "contacts", 0, "relationship") = "spouse"
^patient(123, "labs", "CBC", "value") = 12.5
^patient(123, "labs", "CBC", "unit") = "g/dL"
^patient(123, "labs", "CBC", "date") = 1699900000
```

---

## Open Questions

### 1. Key Ordering

Should key field order be:
- **Explicit only**: Always require `#[rumps(key, order = N)]`
- **Implicit from struct**: Use declaration order, allow override
- **Alphabetical**: Sort by field name (predictable but maybe surprising)

**Recommendation**: Implicit from struct declaration order, with optional `order` override.

### 2. Optional Fields

How to handle `Option<T>`:
- **Omit when None**: Don't store anything, reconstruct as `None` when missing
- **Sentinel value**: Store a special marker for `None`
- **Configurable**: `#[rumps(none = "omit")]` vs `#[rumps(none = "sentinel")]`

**Recommendation**: Omit when `None` by default.

### 3. Field Naming

Default subscript name strategy:
- **Exact**: Use field name as-is (`date_of_birth` → `"date_of_birth"`)
- **Abbreviated**: Some automatic shortening?

**Recommendation**: Exact by default, use `#[rumps(rename = "...")]` for custom.

### 4. Validation on Read

Should `FromRumps` require all fields present?
- **Strict**: Error if any non-`Option` field is missing
- **Lenient**: Use `Default::default()` for missing fields
- **Configurable**: Per-field `#[rumps(default)]` or `#[rumps(default = expr)]`

**Recommendation**: Strict by default, with `#[rumps(default)]` opt-in.

### 5. Schema Versioning

Any support for schema evolution?
- Field additions (easy with "omit None" + defaults)
- Field removals (orphaned data?)
- Field renames (migration path?)
- Type changes (???)

**Recommendation**: Punt on this initially. Document that schema changes are manual.

### 6. Derive Macro Crate Structure

Options:
- `rumps-derive` crate with proc macros
- Feature-gated in `rumps-types` or `rumps-storage`
- Separate `rumps-orm` crate that depends on storage

**Recommendation**: `rumps-derive` crate, re-exported from `rumps-storage` or a future `rumps` facade crate.

### 7. Async Considerations

The extension traits may need async variants:

```rust
#[async_trait]
pub trait DatabaseReadExtAsync: private::Sealed {
    async fn get<T: FromRumps>(&self, key: impl IntoKey) -> Result<Option<T>, Error>;
    // ...
}
```

Or use `impl Future` return types if we want to avoid the `async_trait` macro.

### 8. Streaming Large Collections

For `Vec<T>` with many elements, should there be a streaming API?

```rust
async fn iter_field<T, F: FromRumps>(
    &self,
    key: impl IntoKey,
    field: &str
) -> Result<impl Stream<Item = Result<F, Error>>, Error>;
```

This would allow iterating `patient.visits` without loading all into memory.

---

## Implementation Phases

### Phase 1: Core Traits
- [ ] Define `ToSubscript`, `FromSubscript` in `rumps-types`
- [ ] Define `ToValue`, `FromValue` in `rumps-types`
- [ ] Implement for primitive types
- [ ] Define `DecodeError` type

### Phase 2: Tree Traits
- [ ] Define `ToRumps`, `FromRumps` in `rumps-storage`
- [ ] Define `IntoKey` helper trait
- [ ] Manual implementations for test structs

### Phase 3: Extension Traits
- [ ] Define `DatabaseReadExt` trait
- [ ] Define `TransactionWriteExt` trait
- [ ] Implement for `Database` and `Transaction`

### Phase 4: Derive Macros
- [ ] Create `rumps-derive` crate
- [ ] Implement `#[derive(ToRumps)]`
- [ ] Implement `#[derive(FromRumps)]`
- [ ] Implement `#[derive(ToValue, FromValue)]` for unit enums
- [ ] Support all field attributes

### Phase 5: Polish
- [ ] Error messages and diagnostics
- [ ] Documentation
- [ ] Integration tests
- [ ] Benchmarks

---

## Related Work / Inspiration

- **serde**: General serialization framework, attribute syntax
- **diesel**: Rust ORM, query builder patterns
- **sea-orm**: Async ORM, derive macros
- **sqlx**: Compile-time checked queries (less relevant but good patterns)

---

## Recommended Approach

Summary of my recommendations for getting started quickly:

### Open Questions: My Picks

| Question              | Recommendation                   | Rationale                                                   |
|-----------------------|----------------------------------|-------------------------------------------------------------|
| Key ordering          | Implicit from struct field order | Matches user intuition; `order` attribute for edge cases    |
| Optional fields       | Omit when `None`                 | Sparse storage is RUMPS's strength; no wasted space         |
| Field naming          | Exact field name                 | Predictable; `rename` for abbreviation when needed          |
| Validation on read    | Strict + `#[rumps(default)]`     | Catches bugs early; opt-in leniency                         |
| Schema versioning     | Punt                             | Out of scope for v1; document manual migration              |
| Crate structure       | `rumps-derive` crate             | Clean separation; re-export from facade                     |
| Async                 | Match existing API               | If `Database`/`Transaction` are async, traits should be too |
| Streaming collections | Defer to v2                      | Nice-to-have; `Vec<T>` loading is fine for most cases       |

### Enum Strategy

Use a **tiered approach**:

1. **Unit enums** → `ToValue`/`FromValue` (stored as string discriminant)
2. **Data enums** → Default to **externally tagged** (variant name as subscript)
3. Allow `#[rumps(tag = "type")]` for adjacently tagged when explicit

Externally tagged is simplest to implement and query (you can `$ORDER` over variants).

### Trait Design: Keep It Simple

Start with the minimal viable trait set:

```rust
// rumps-types
pub trait ToSubscript { fn to_sub(&self) -> Subscript; }
pub trait FromSubscript { fn from_sub(s: &Subscript) -> Result<Self, Error>; }
pub trait ToValue { fn to_val(&self) -> Value; }
pub trait FromValue { fn from_val(v: &Value) -> Result<Self, Error>; }

// rumps-storage
pub trait ToRumps {
    const GLOBAL: &'static str;

    fn to_key(&self) -> Key;
    fn to_pairs(&self, prefix: &Key) -> Vec<(Key, Value)>;
}

pub trait FromRumps: Sized {
    const GLOBAL: &'static str;

    fn from_pairs<I: Iterator<Item = (Key, Value)>>(prefix: &Key, pairs: I) -> Result<Self, Error>;
}
```

Avoid over-engineering:
- No `KeyPattern` type initially—just use `Key` as prefix
- No separate `key_pattern()` method—derive it from the type's key fields
- Single `Error` type, not multiple error variants

### Extension Traits: Enforce Transaction Safety

This is the key ergonomic win. The split is important:

```rust
// Reads: available everywhere
#[async_trait]
pub trait RumpsRead: private::Sealed {
    async fn get<T: FromRumps>(&self, key: impl IntoKey) -> Result<Option<T>, Error>;
    async fn all<T: FromRumps>(&self) -> Result<Vec<T>, Error>;  // Vec for simplicity initially
    async fn exists<T: FromRumps>(&self, key: impl IntoKey) -> Result<bool, Error>;
}

// Writes: Transaction only (compile-time enforcement!)
#[async_trait]
pub trait RumpsWrite: private::Sealed {
    async fn insert<T: ToRumps>(&mut self, val: &T) -> Result<(), Error>;
    async fn delete<T: ToRumps>(&mut self, key: impl IntoKey) -> Result<(), Error>;
    async fn upsert<T: ToRumps>(&mut self, val: &T) -> Result<(), Error>;
}
```

Skip `update` initially—users can `get` + modify + `upsert`. Less magic, fewer edge cases.

### Derive Macro: Start Minimal

Phase 1 derive macro should support only:

```rust
#[derive(ToRumps, FromRumps)]
#[rumps(global = "patient")]
struct Patient {
    #[rumps(key)]           // Required for at least one field
    id: u64,
    name: String,           // Primitives only
    age: u32,
}
```

Defer for later:
- `#[rumps(flatten)]` — requires recursive expansion
- `#[rumps(subtree)]` — needs nested struct handling
- Collections (`Vec<T>`, `HashMap<K,V>`) — complex iteration logic
- Composite keys with `order` — single key is enough to start
- Enums — start with `ToValue` for unit enums only

### Suggested Implementation Order

1. **`ToSubscript`/`FromSubscript`** for primitives (trivial, test the pattern)
2. **`ToValue`/`FromValue`** for primitives (similar)
3. **`IntoKey`** tuple impls (mechanical but important for ergonomics)
4. **Manual `ToRumps`/`FromRumps`** impl for a test struct (validate the design)
5. **`RumpsRead` trait** + impl for `Database` (biggest value-add)
6. **`RumpsWrite` trait** + impl for `Transaction`
7. **`#[derive(ToRumps)]`** — codegen for step 4
8. **`#[derive(FromRumps)]`** — inverse of step 7

### Code Organization

```
rumps-types/src/
  lib.rs
  subscript.rs      # ToSubscript, FromSubscript
  value.rs          # ToValue, FromValue (add to existing?)

rumps-storage/src/
  lib.rs
  orm/
    mod.rs          # Re-exports
    traits.rs       # ToRumps, FromRumps, IntoKey
    ext.rs          # RumpsRead, RumpsWrite extension traits
    sealed.rs       # private::Sealed

rumps-derive/       # New crate
  Cargo.toml
  src/
    lib.rs          # proc_macro_derive entry points
    to_rumps.rs     # ToRumps derive impl
    from_rumps.rs   # FromRumps derive impl
```

### What Success Looks Like

The MVP is complete when this compiles and works:

```rust
use rumps_storage::{Database, ToRumps, FromRumps, RumpsRead, RumpsWrite};

#[derive(ToRumps, FromRumps)]
#[rumps(global = "user")]
struct User {
    #[rumps(key)]
    id: u64,
    name: String,
    email: String,
}

let db = Database::open("./data")?;

// Write requires transaction
db.transaction(|txn| async {
    txn.insert(&User { id: 1, name: "Alice".into(), email: "a@b.c".into() }).await?;
    Ok(())
}).await?;

// Read works on db directly
let user: Option<User> = db.get(1).await?;
let users: Vec<User> = db.all().await?;
```

Everything else is iteration on this foundation.
