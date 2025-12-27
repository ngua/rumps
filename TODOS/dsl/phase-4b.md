## Phase 4.14: Integration and Entry Point

Hook type checking into the interpreter pipeline.

### Entry Point (`typecheck.rs`)

```rust
pub(crate) fn check(ast: &Ast, registry: &TypeRegistry, env: &Environment) -> crate::Result<()> {
    // Builtin types come from `env`; no separate registration needed
    let mut ctx = InferCtx::new(ast, registry, env);

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

**Note**: `InferCtx` receives `&Environment` and queries `env.get_module_fn_type()` when type-checking module function calls. No separate builtin registration step.

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

- [x] Type checker: handle `Expr::Path` (module function references):
  - [x] Look up scheme via `env.get_module_fn_type(path)`
  - [x] Instantiate scheme with fresh type variables (`scheme.instantiate(&mut self.next_var)`)
  - [x] Return the instantiated `Ty::Fn` (not `Ty::Unknown`)
  - [x] This allows `Callable` constraint solving to unify type variables with concrete arg types
- [x] Handle `Stmt::Union` in `stmt` (no-op; registry handles registration)
- [ ] Resolve user-defined union members in `expand_union_members` → **Deferred to Phase 4.14.1**
- [x] Add `pub(crate) fn check(ast, registry, env) -> crate::Result<()>` to `typecheck.rs`
- [x] `impl InferCtx`: `fn into_result(self) -> crate::Result<()>`
- [ ] `impl TypeExprArena`: `fn from_ty(&mut self, ty: &Ty, registry: &TypeRegistry) -> TypeExprId` → **Not needed yet**
- [x] Modify `crates/rumps-query/src/lib.rs`:
  - [x] Add `mod typecheck;`
- [x] Modify `crates/rumps-query/src/interpreter.rs`:
  - [x] Call `crate::typecheck::check(ast, &registry, &env)?` after resolution
- [x] Modify `crates/rumps-query/src/error.rs`:
  - [x] Add `Error::StaticType(TypeError)` variant for compile-time type errors
  - [x] Add `Error::static_types(Vec<TypeError>) -> Self` constructor:
    - [x] If single error, return `Error::StaticType(err)`
    - [x] If multiple, wrap in `Error::Multiple`
  - [x] Update `Diagnostic` impl: add `"rumps::static_type"` code for new variant
  - [x] Update `ErrorDisplay` impl for new variant
  - [x] Update all call sites of `Error::type_err()` to `Error::runtime_type()`

### Issues Fixed After Initial Implementation

1. **Stack overflow in `Ty::apply`**: `Scheme::poly` uses `TyVar(0)`, but `InferCtx.next_var` also starts at `0`. First polymorphic instantiation created `{TyVar(0) -> Var(TyVar(0))}` causing infinite recursion. **Fix**: Filter identity mappings in `Scheme::instantiate`.

2. **Array module type signatures backwards**: Type signatures had `(Array[T], T -> U)` but scripts use `(T -> U, Array[T])`. **Fix**: Swapped argument order for `map`, `filter`, `reduce`, `foreach`.

3. **Closure parameter inference failing**: `x => x * 2` didn't infer `x: Int` even though `2` is `Int`. The `binary` function added `Numeric` constraints but never unified type vars with concrete `Int`. **Fix**: In arithmetic ops, if one operand is concrete `Int`, unify the other with `Int`.

4. **Optional chaining `?.` required `Option` base**: `obj?.field` expected `obj` to be `Option[T]`, but it should work on any object and wrap the result in `Option`. **Fix**: Handle `Object` and `Named` types directly in `optional_field`, wrapping field type in `Option`.

5. **Tests expected old broken behavior**: `infer_add_int_int` and `infer_add_with_var_produces_fresh_numeric` expected `Numeric` constraints even for concrete `Int` operands, and expected type variables as results when one operand was `Int`. Updated tests to expect the new correct behavior: `Int + Int = Int` directly, `?var + Int` unifies `?var` with `Int`.

---

## Phase 4.14.1: Test Script Audit and Fixes

The type checker is producing incorrect errors on existing test scripts. This phase audits each script, identifies the root causes, and fixes them.

### Remaining Work

For each of the ~46 failing test scripts:

1. **Run the script** and examine the type checker output
2. **Categorize the errors**:
   - **Legitimate type errors**: The type checker correctly caught a violation (e.g., `33_type_mismatch.rumps`). Update the snapshot to include the expected error.
   - **Broken scripts**: Some scripts may be testing something non-sensical, e.g. `SET x = 100 \n OUTPUT x`; this _shouldn't_ work and the script is broken (i.e. non-type error, but still a broken script)
   - **Type checker bugs**: The type checker incorrectly rejects valid code. Fix the bug.
3. **Fix or update snapshot**: Either fix the type checker issue or run `cargo insta review` to accept the new snapshot if the errors are expected.
   - **NOTE**: Do NOT introduce new regressions in other scripts. That is not a fix. If a script that is NOT in the audit checklist below, and it is broken following your fixes, you must reconsider your approach or ask for guidance
3. **Stop there**: Do not continue in a loop. Wait for instructions after fixing each script.

#### Audit Checklist

| Script                         | Status | Category    | Notes                                                       |
|--------------------------------|--------|-------------|-------------------------------------------------------------|
| `87_union_types`               | PASS   | **Fixed**   | Fixed in Phase 4.14.2                                       |

**NOTE**: All scripts now pass

#### Success Criteria

All test scripts pass with:
1. No spurious type errors (false positives)
2. Genuine type errors caught where expected (error scripts like `33_type_mismatch.rumps`)
3. Snapshots updated only where the new behavior is correct


### Completed Fixes

#### 1. `Json` Type Handling ✓

All JSON operations now type-check correctly:

| Operation       | Type Signature                |
|-----------------|-------------------------------|
| `json.field`    | `Json -> Json`                |
| `json[idx]`     | `Json -> Json`                |
| `json..field`   | `Json -> Option[Scalar]`      |
| `json->(expr)`  | `Json -> Json`                |
| `json->>(expr)` | `Json -> Option[Scalar]`      |
| `val AS Json`   | `T -> Json` (always succeeds) |
| `json READ T`   | `Json -> Result[T, String]`   |

#### 2. Heterogeneous Arrays ✓

Arrays with mixed element types correctly infer to `Json`:
```rumps
LET arr = [1, "two", TRUE]  ; Correctly typed as Json
LET mixed = ['a', 2, "string"]  ; Correctly typed as Json
```

#### 3. HasField Constraint ✓

Field access on type variables no longer incorrectly requires all struct fields.
Previously, `person.name` would generate `Eq(person_ty, Object({name: ?t}))`, which
failed when `person_ty` resolved to a struct with more fields. Now uses `HasField`
constraint that only checks the accessed field exists.

#### 4. Polymorphic Option/Result ✓

`Option.None` and similar polymorphic values no longer cause "type annotation required"
errors. Unresolved type parameters inside `Option` and `Result` are allowed since
they don't affect runtime behavior.

#### 5. User-Defined Type Registration ✓

User-defined types (via `TYPE Name = ...`) are now registered before type checking,
allowing the type checker to look up struct field types for field access validation.

#### 6. Object Compatibility in `types_compatible` ✓

Arrays of objects were incorrectly typed as `Json` because `types_compatible` (used
to detect heterogeneous arrays) lacked handling for `Ty::Object`. Fixed by adding
structural comparison: same field names with pairwise compatible field types.
Also added handling for `Ty::Result`.

#### 7. Polymorphic Empty Arrays ✓

Empty arrays `[]` no longer require type annotations. Changed `has_unresolved_vars`
to treat `Array[T]` like `Option[T]` and `Result[T,E]`; polymorphic container types
with unresolved element types don't require annotation.

#### 8. Range-to-Array Coercion ✓

`Range` now coerces to `Array[Int]` during unification, allowing Array HOFs like
`Array.foreach(f, 1..10)` to work without explicit conversion.

#### 9. Numeric Defaulting ✓

Unresolved numeric type variables now default to `Int` (like Haskell's defaulting
rules). When `check_numeric` sees `Numeric(Ty::Var(v))` where `v` is unresolved,
it extends the substitution with `v -> Int`. This enables inference for closures
like `x => x + 1` passed to HOFs with polymorphic empty arrays.

#### 10. Polymorphic Type Variables ✓

`has_unresolved_vars` now allows:
- `Ty::Var(_)`: Unresolved type variables are OK for polymorphic expressions
- `Ty::Map(..)`: Empty maps are polymorphic like empty arrays
- `Ty::Fn(..)`: Closures passed to HOFs inherit polymorphism from their context

Only `Ty::Unknown` (true ambiguity) now triggers `MissingAnnotation` errors.
This allows natural polymorphic code like `Array.map(x => Option.Some(x), [])`
without requiring type annotations.

### Implementation Details (Completed)

Key changes:
- Added `HasField` constraint for field access on type variables
- Added `Iterable` constraint for future iterable collection support
- Process `Unwrappable`, `HasField`, and `Iterable` constraints in first pass (with `Eq`)
- Allow unresolved type vars in `Option`/`Result`/`Array`/`Map`/`Fn` types
- Register user types in `register_from_ast` before type checking
- Make runtime `type_decl` idempotent (skip if already registered)
- Add `Ty::Object` and `Ty::Result` handling to `types_compatible`
- Add Range-to-Array coercion in `unify_inner`
- Default unresolved `Numeric` type variables to `Int` in `check_numeric`
- Allow bare `Ty::Var` in `has_unresolved_vars` (only `Ty::Unknown` requires annotation)

---

## Phase 4.14.2: User-Defined Union Support

The type checker currently skips user-defined unions because their members are stored as `TypeExprId`s (runtime type references) rather than `Ty` (static types). This phase adds the missing infrastructure.

### Problem

```rust
// In TypeDef::Union
TypeDef::Union {
    name: StringId,
    type_params: SmallVec<[StringId; 2]>,
    members: Vec<TypeExprId>,  // Runtime type refs, not Ty!
}
```

When `expand_union_members` encounters a user-defined union, it finds `TypeDef::Union` but can't convert `TypeExprId -> Ty` without the `TypeExprArena`.

### Solution

Add `TypeExprArena` reference to `InferCtx` and implement `TypeExpr -> Ty` conversion.

### Implementation

#### 1. Extend `InferCtx` with `TypeExprArena`

```rust
pub(crate) struct InferCtx<'a> {
    ast: &'a Ast,
    registry: &'a TypeRegistry,
    type_exprs: &'a TypeExprArena,  // NEW
    // ...
}

impl<'a> InferCtx<'a> {
    pub(crate) fn new(
        ast: &'a Ast,
        registry: &'a TypeRegistry,
        type_exprs: &'a TypeExprArena,  // NEW
        env: &'a Environment,
        strings: &'a StringArena,
    ) -> Self {
        // ...
    }
}
```

#### 2. Add `TypeExpr -> Ty` conversion

```rust
impl InferCtx<'_> {
    /// Convert a runtime `TypeExprId` to a static `Ty`.
    fn type_expr_to_ty(&self, id: TypeExprId) -> Ty {
        match self.type_exprs.get(id) {
            Some(TypeExpr::Named(type_id)) => self.type_id_to_ty(*type_id),
            Some(TypeExpr::App { base, args }) => {
                let base_ty = self.type_id_to_ty(*base);
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|a| self.type_expr_to_ty(*a))
                    .collect();
                self.apply_type_args(base_ty, arg_tys)
            }
            Some(TypeExpr::Object(fields)) => {
                let field_tys: IndexMap<StringId, Ty> = fields
                    .iter()
                    .map(|(k, v)| (*k, self.type_expr_to_ty(*v)))
                    .collect();
                Ty::Object(field_tys)
            }
            None => Ty::Unknown,
        }
    }

    /// Convert a `TypeId` to primitive `Ty` or `Ty::Named`.
    fn type_id_to_ty(&self, id: TypeId) -> Ty {
        match id {
            TypeId::BOOL => Ty::Bool,
            TypeId::INT => Ty::Int,
            TypeId::FLOAT => Ty::Float,
            TypeId::CHAR => Ty::Char,
            TypeId::STRING => Ty::String,
            TypeId::UNIT => Ty::Unit,
            TypeId::TIME => Ty::Time,
            TypeId::RANGE => Ty::Range,
            TypeId::JSON => Ty::Json,
            // For parameterized or user-defined types, use Named
            _ => Ty::Named(id, vec![]),
        }
    }

    /// Apply type arguments to a base type.
    fn apply_type_args(&self, base: Ty, args: Vec<Ty>) -> Ty {
        match base {
            Ty::Named(id, _) if id == TypeId::ARRAY && args.len() == 1 => {
                Ty::Array(Box::new(args.into_iter().next().unwrap()))
            }
            Ty::Named(id, _) if id == TypeId::OPTION && args.len() == 1 => {
                Ty::Option(Box::new(args.into_iter().next().unwrap()))
            }
            Ty::Named(id, _) if id == TypeId::MAP && args.len() == 2 => {
                let mut it = args.into_iter();
                Ty::Map(Box::new(it.next().unwrap()), Box::new(it.next().unwrap()))
            }
            Ty::Named(id, _) if id == TypeId::RESULT && args.len() == 2 => {
                let mut it = args.into_iter();
                Ty::Result(Box::new(it.next().unwrap()), Box::new(it.next().unwrap()))
            }
            Ty::Named(id, _) if id == TypeId::TUPLE => Ty::Tuple(args),
            Ty::Named(id, _) => Ty::Named(id, args),
            _ => base,
        }
    }
}
```

#### 3. Update `expand_union_members`

```rust
pub(crate) fn expand_union_members(&self, ty: &Ty) -> Option<Vec<Ty>> {
    match ty {
        Ty::Union(members) => Some(members.clone()),
        Ty::Named(id, _params) => {
            if *id == TypeId::STORABLE {
                Some(Ty::STORABLE_MEMBERS.to_vec())
            } else if *id == TypeId::SCALAR {
                Some(Ty::SCALAR_MEMBERS.to_vec())
            } else {
                self.registry.get_def(*id).and_then(|def| match def {
                    TypeDef::Union { members, .. } => {
                        // Convert each TypeExprId to Ty
                        Some(
                            members
                                .iter()
                                .map(|m| self.type_expr_to_ty(*m))
                                .collect(),
                        )
                    }
                    _ => None,
                })
            }
        }
        _ => None,
    }
}
```

#### 4. Update `check()` entry point

```rust
pub(crate) fn check(
    ast: &Ast,
    stmts: &[StmtId],
    registry: &TypeRegistry,
    type_exprs: &TypeExprArena,  // NEW
    env: &Environment,
    strings: &StringArena,
) -> crate::Result<()> {
    let mut ctx = InferCtx::new(ast, registry, type_exprs, env, strings);
    // ...
}
```

### Checklist

- [x] Add `type_exprs: &'a TypeExprArena` field to `InferCtx`
- [x] Update `InferCtx::new` to accept `TypeExprArena`
- [x] Implement `type_expr_to_ty(TypeExprId) -> Ty`
- [x] Implement `type_id_to_ty(TypeId) -> Ty`
- [x] Implement `apply_type_args(Ty, Vec<Ty>) -> Ty`
- [x] Update `expand_union_members` to use `type_expr_to_ty` for user-defined unions
- [x] Update `check()` signature to accept `TypeExprArena`
- [x] Update interpreter call site to pass `TypeExprArena`
- [x] Add test script with user-defined union and match (existing `87_union_types.rumps`)
- [x] Add test: `UNION Num = Int | Float` then `MATCH x { IS Int => ..., IS Float => ... }` (covered in `87_union_types.rumps`)
- [x] Verify exhaustiveness checking works for user-defined unions
- [x] Make sure `87_union_types` script is working again

### Test Script

```rumps
; scripts/80_user_union.rumps
UNION NumOrStr = Int | String

LET x: NumOrStr = 42

; Pattern matching should validate members
LET result = MATCH x {
    IS Int => "got int"
    IS String => "got string"
}

PRINT result
```

---

## Phase 4.15: Error Messages and Diagnostics

Improve error messages with suggestions and context.

**NOTE**: Type errors are currently _awful_.

### Checklist

- [x] Add span information to all error types
- [x] Implement `Display` for `Ty` (pretty-print types)
- [x] Add "expected X, got Y" format for mismatches
- [x] Add suggestions for common mistakes (not very important)
- [x] Integrate with `miette` for nice error rendering

---

## Phase 4.16: Testing

Comprehensive test suite for the type checker. **91 integration test scripts** cover all categories.

### Test Categories and Coverage

| Category                   | Status | Scripts                                                                                      |
|----------------------------|--------|----------------------------------------------------------------------------------------------|
| **Literals and variables** | ✅     | `01`, `02`, `03`, `32`, `89` (Int, Float, String, Bool, Char, Unit)                          |
| **Operators**              | ✅     | `01`, `02`, `09`, `10`, `11`, `21`, `22`, `36` (numeric, comparison, logical, string, power) |
| **Collections**            | ✅     | `15`, `18`, `19`, `23`, `46`, `77`, `88` (array, tuple, object, map, nested)                 |
| **Functions**              | ✅     | `37`, `38`, `39`, `71`, `72` (closures, calls, higher-order, recursion)                      |
| **Control flow**           | ✅     | `08`, `12`, `27`, `62` (if/else, match, blocks, guards)                                      |
| **Variants**               | ✅     | `25`, `29`, `53`, `56`, `62`, `79`, `87` (Option, Result, user-defined)                      |
| **Errors**                 | ✅     | `33`, `40`-`44`, `48`-`52`, `54`-`55`, `57`-`61`, `63`-`65`, `70`, `72`, `83`-`85`, `90`     |
| **Inference**              | ✅     | `91` (polymorphic functions, generalization, identity, compose)                              |
| **Database**               | ✅     | `06` (GET with globals, transactions)                                                        |

### Checklist

- [x] Add integration tests for expression inference (covered by `01`-`03`, `32`, etc.)
- [x] Add integration tests for statement inference (covered by `03`, `06`, `38`, etc.)
- [x] Add integration tests for unification (covered by `71`, `91`)
- [x] Add error case tests (covered by `33`, `40`-`44`, `48`-`52`, `90`, etc.)
- [x] Add polymorphism tests (`91_polymorphic_inference.rumps`)

### New Scripts Added

- `90_undefined_var_error.rumps`: Tests that undefined variable errors are caught
- `91_polymorphic_inference.rumps`: Tests polymorphic function generalization (identity, const, flip, compose, apply-twice, etc.)

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

**Benefits:**
- Consistent error messages across the codebase
- Easy to grep for all type-checker-guaranteed branches
- Single point of change if we want to modify the message format
- Self-documenting: the macro name makes intent clear

**Placement:** Define in `crates/rumps-query/src/interpreter/mod.rs` or a shared `macros.rs` module.

### Checklist

- [x] Remove `check_arity` function entirely
- [x] Remove all `check_arity` calls (~50+ sites across all module functions)
- [x] Array module functions (`Array.length`, `Array.map`, etc.)
- [x] String module functions (`String.length`, `String.split`, etc.)
- [x] Math module functions (`Math.abs`, `Math.floor`, etc.)
- [x] Map module functions (`Map.length`, `Map.keys`, etc.)
- [x] Time module functions (`Time.now`, `Time.parse`, etc.)
- [x] Random module functions (`Random.int`, `Random.choice`, etc.)
- [x] Option/Result module functions (`Option.unwrap-or`, `Result.map`, etc.)
- [x] `array_elems()`: Remove type equality check
- [x] `map_lit_entries()`: Remove key type homogeneity check
- [x] `map_lit_entries()`: Remove value type homogeneity check
- [x] `array_map_rec()`: Remove result type homogeneity check
- [x] `range_map_rec()`: Remove result type homogeneity check
- [x] `array_filter_rec()`: Remove similar checks if present
- [x] `array_reduce()`: Verify no type checks needed
- [x] `apply_unop()`: Remove error branches for `-` and `!`
- [x] `binop_add()`: Remove error branch
- [x] `binop_sub()`: Remove error branch
- [x] `binop_mul()`: Remove error branch
- [x] `binop_div()`: Remove error branch
- [x] `binop_floor_div()`: Remove error branch
- [x] `binop_mod()`: Remove error branch
- [x] `binop_pow()`: Remove error branch
- [x] `binop_cmp()`: Remove error branch for comparison operators
- [x] Change return types from `Result<Value>` to `Value` where possible
- [x] Remove `check_unit()` function entirely
- [x] `r#if`: Remove `check_unit` call
- [x] `if_with_bindings`: Remove `check_unit` call
- [x] `r#if`: Remove `Bool` check on condition (type checker guarantees `Bool`)
- [x] `r#match`:
  - [x] Remove all exhaustiveness checking (already statically guaranteed)
  - [x] `try_match_arms`: Remove `Bool` check on guard (type checker guarantees `Bool`)
- [x] `array_filter_rec`: Remove `Bool` check on predicate result (type checker guarantees `Bool`)
- [x] `range_filter_rec`: Remove `Bool` check on predicate result (type checker guarantees `Bool`)
- [x] `coalesce()`: Remove type error branch
- [x] `range()`: Remove start type check
- [x] `range()`: Remove end type check
- [x] `to_float()`: Remove error branch, change return to `f64`
- [x] `to_int()`: Remove error branch, change return to `i64`
- [x] Update all call sites of `to_float`/`to_int` to remove `?`


**RUMPS Primitives by Module:**
- [x] `Array.length`: Remove Array type check
- [x] `Array.head`: Remove Array type check
- [x] `Array.tail`: Remove Array type check
- [x] `Array.last`: Remove Array type check
- [x] `Array.init`: Remove Array type check
- [x] `Array.nth`: Remove Array type check
- [x] `Array.reverse`: Remove Array type check
- [x] `Array.concat`: Remove Array type checks
- [x] `Array.contains`: Remove Array type check
- [x] `Array.map`: Remove Array/closure type checks
- [x] `Array.filter`: Remove Array/closure type checks
- [x] `Array.reduce`: Remove Array/closure type checks
- [x] `Array.find`: Remove type checks
- [x] `Array.any`: Remove type checks
- [x] `Array.all`: Remove type checks
- [x] `Array.sort`: Remove type checks
- [x] `Array.sort-by`: Remove type checks
- [x] `String.length`: Remove String type check
- [x] `String.chars`: Remove String type check
- [x] `String.split`: Remove String type checks
- [x] `String.join`: Remove Array/String type checks
- [x] `String.trim`: Remove String type check
- [x] `String.starts-with`: Remove String type checks
- [x] `String.ends-with`: Remove String type checks
- [x] `String.contains`: Remove String type checks
- [x] `String.replace`: Remove String type checks
- [x] `String.to-upper`: Remove String type check
- [x] `String.to-lower`: Remove String type check
- [x] `String.pad-left`: Remove type checks
- [x] `String.pad-right`: Remove type checks
- [x] All Math functions: Remove Float type checks
- [x] `Math.abs`, `Math.floor`, `Math.ceil`, `Math.round`, etc.
- [x] `Map.length`: Remove Map type check
- [x] `Map.keys`: Remove Map type check
- [x] `Map.values`: Remove Map type check
- [x] `Map.entries`: Remove Map type check
- [x] `Map.has`: Remove Map type check
- [x] `Map.lookup`: Remove Map type check
- [x] `Map.insert`: Remove Map type check
- [x] `Map.remove`: Remove Map type check
- [x] `Map.merge`: Remove Map type checks
- [x] `Map.from-entries`: Remove Array type check
- [x] `Time.now`: No type checks needed
- [x] `Time.parse`: Remove String type check
- [x] `Time.format`: Remove Time/String type checks
- [x] `Time.add-*`: Remove Time/Int type checks
- [x] `Time.diff-*`: Remove Time type checks
- [x] `Random.int`: Remove Int type checks
- [x] `Random.float`: Remove Float type checks
- [x] `Random.choice`: Remove Array type check
- [x] `Random.shuffle`: Remove Array type check
- [x] `Option.unwrap-or`: Remove Option type check
- [x] `Result.unwrap-or`: Remove Result type check
- [x] `Option.map`: Remove type checks
- [x] `Result.map`: Remove type checks
- [x] `Result.map-err`: Remove type checks
- [x] `check_variant_zero_arity()`: Remove runtime arity validation
- [x] `check_variant()`: Simplify type/variant matching
- [x] Pattern exhaustiveness is checked statically; remove runtime fallbacks
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

**Expected benefits:**
- Smaller binary size (less error handling code)
- Faster execution (no redundant checks)
- Cleaner code (pattern matches without error branches)
- Confidence: each removed check has a test proving the type-checker catches it

---

## Phase 4.18: Type Checker Cleanup and Module Reorganization

Remove development-time `#[allow(...)]` annotations and split the monolithic `infer.rs` (7400+ lines) into focused submodules.

### 4.18.1: Remove `#[allow(...)]` Annotations

**Current state:**
```rust
// crates/rumps-query/src/typecheck/infer.rs:7
#![allow(clippy::large_enum_variant, dead_code)]
```

**Checklist:**
- [ ] Remove `#![allow(clippy::large_enum_variant, dead_code)]` from `infer.rs`
- [ ] Fix or remove any resulting dead code warnings
- [ ] Address `large_enum_variant` if clippy flags it (consider `Box`ing large variants)
- [ ] Audit other typecheck modules for stray `#[allow(...)]` annotations

---

### 4.18.2: Split `infer.rs` into Submodules

The `infer.rs` file is ~7400 lines with tests comprising nearly two-thirds. Split into focused modules using the modern `module.rs` pattern (NOT `mod.rs`).

**Target structure:**
```
crates/rumps-query/src/typecheck/
├── infer.rs          # Re-exports, Constraint enum, InferCtx struct definition
├── infer/
│   ├── expr.rs       # Expression inference (literals, binary/unary, collections, access)
│   ├── stmt.rs       # Statement inference (let, fun, set, kill, output)
│   ├── pattern.rs    # Pattern matching, exhaustiveness checking, bind_pattern
│   ├── convert.rs    # Type conversions (ast_type_to_ty, type_expr_to_ty, etc.)
│   └── tests.rs      # All unit tests (~4700 lines)
├── env.rs
├── error.rs
├── ty.rs
└── unify.rs
```

**Module contents:**

| Module       | Contents                                                                                  | ~Lines |
|--------------|-------------------------------------------------------------------------------------------|--------|
| `infer.rs`   | `Constraint` enum, `InferCtx` struct, `new()`, `infer()`, `stmt()`, re-exports            | ~400   |
| `expr.rs`    | `expr_inner`, `literal`, `var`, `binary`, `unary`, `array`, `tuple`, `object`, `map_lit`, `field`, `optional_field`, `tuple_index`, `index`, `json_access`, `closure`, `call`, `r#if`, `block`, `r#match`, `match_arm`, `join_types`, `variant`, `unwrap`, `is_check`, `as_cast`, `read_conv`, `get`, `annotate` | ~1800 |
| `stmt.rs`    | `fun`, `r#let`, `bind_pattern`, `set`, `kill`, `output`                                   | ~200   |
| `pattern.rs` | `pattern_bindings`, `check_exhaustiveness`, `is_irrefutable_pattern`, `variant_payload_types`, `expand_union_members`, `is_union_member` | ~400   |
| `convert.rs` | `ast_type_to_ty`, `type_expr_to_ty`, `type_id_to_ty`, `apply_type_args`, `named_type_to_ty`, `parameterized_type_to_ty`, `field_type`, `types_compatible`, `has_unresolved_vars` | ~350 |
| `tests.rs`   | All `#[cfg(test)] mod tests { ... }`                                                      | ~4700  |

**Note on tests:** The current unit tests use a limited `TestState` struct that manually constructs partial AST nodes and doesn't exercise real behavior. These tests should be rewritten to use the full pipeline: `parse -> CST -> AST -> typecheck`. This ensures tests validate actual type inference on real source code rather than synthetic AST fragments. Consider:
- Replacing `TestState` with a helper that parses source strings
- Using integration-style tests that mirror the `scripts/*.rumps` approach
- Removing tests that only verify internal implementation details

**Implementation approach:**

1. Create `infer/` directory and submodule files
2. Move `#[cfg(test)] mod tests` to `infer/tests.rs`
3. Extract type conversion methods to `infer/convert.rs`
4. Extract pattern matching logic to `infer/pattern.rs`
5. Extract statement inference to `infer/stmt.rs`
6. Extract expression inference to `infer/expr.rs`
7. Keep `Constraint`, `InferCtx` struct definition, and dispatch methods in `infer.rs`
8. Use `impl InferCtx<'_>` blocks in each submodule (Rust allows multiple impl blocks)

**Checklist:**
- [x] Create `crates/rumps-query/src/typecheck/infer/` directory
- [x] Create `infer/tests.rs`; move all test code
- [x] Create `infer/convert.rs`; move type conversion methods
- [x] Create `infer/pattern.rs`; move pattern matching logic
- [x] Create `infer/stmt.rs`; move statement inference
- [x] Create `infer/expr.rs`; move expression inference
- [x] Update `infer.rs` to declare submodules: `mod convert; mod expr; mod pattern; mod stmt;`
- [x] Add `#[cfg(test)] mod tests;` to `infer.rs`
- [x] Verify `cargo test` passes
- [x] Verify `cargo clippy` passes with no new warnings

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

