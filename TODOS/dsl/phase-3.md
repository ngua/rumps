# Phase 3: Collection Operations and Patterns

This document tracks the third phase of implementing the RUMPS query language: regex pattern matching, higher-order collection operations, and related features.

**Prerequisites**: Phase 2 (operators, functions, closures) complete.

**Testing**: Each feature requires both unit tests and integration tests (`.rumps` script + `.expected` output in `tests/scripts/`).

**NOTE**: If integration tests are failing after modifications to parser, etc..., it may be due to outdated snapshots. Use `cargo insta` to fix

## Goals

1. Regex pattern matching (`matches`, `/pattern/`)
2. Higher-order collection operations (`MAP`, `FILTER`, `REDUCE`)

## Phase 3 Tasks

### 1. Regex Pattern Matching (`matches`)

Pattern matching with regex literals.

```rumps
IF email matches /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

IF ssn matches /^\d{3}-\d{2}-\d{4}$/ {
  OUTPUT "Valid SSN format"
}

; Negation
IF input matches! /[<>]/ {
  OUTPUT "No angle brackets"
}
```

- [ ] Add regex literal support to lexer (`/pattern/`)
  - Handle escape sequences (`\/`, `\\`)
  - Consider flags suffix (`/pattern/i` for case-insensitive)
- [ ] Add `Token::Regex(String)` for regex literals
- [ ] Add `Token::Matches` keyword to lexer
- [ ] Add `Expr::Matches(ExprId, String)` to AST (value, pattern)
- [ ] Add `regex` crate dependency
- [ ] Implement in interpreter:
  - Compile regex (cache compiled patterns)
  - Return `true`/`false` for match
- [ ] Add unit tests
- [ ] Add integration test script

**Deferred regex features** (for later):
- Named capture groups (`(?<name>...)`) and `Match.name` access
- `matches!` negation syntax (can use `NOT (x matches /.../)` for now)
- Regex flags (`/pattern/i`, `/pattern/m`)

### 2. Higher-Order Collection Operations

With function types and closures in place from Phase 2, these are now straightforward to implement.

```rumps
MAP (x => x * 2) [1, 2, 3]
FILTER (x => x > 2) [1, 2, 3, 4]
REDUCE (acc, x => acc + x) 0 [1, 2, 3]
```

Type signatures:
```rumps
; MAP signature: ((T) -> U, Array[T]) -> Array[U]
; FILTER signature: ((T) -> Bool, Array[T]) -> Array[T]
; REDUCE signature: ((A, T) -> A, A, Array[T]) -> A
```

- [ ] Implement `MAP` keyword/function
- [ ] Implement `FILTER` keyword/function
- [ ] Implement `REDUCE` keyword/function
- [ ] Add unit tests
- [ ] Add integration test script

## Design Decisions

### Regex Literals

Regex literals use `/pattern/` syntax. The pattern is compiled at first use and cached.

```rumps
/^hello/           ; anchored at start
/world$/           ; anchored at end
/\d{3}-\d{4}/      ; digit patterns
```

The `matches` operator returns a boolean. For capture groups and more advanced features, use a `Regex.match(pattern, string)` function (deferred).

Regex literals cannot span multiple lines. Use string concatenation for complex patterns:

```rumps
LET pattern = "^(" ++ part1 ++ ")|(" ++ part2 ++ ")$"
IF text matches Regex.compile(pattern) { ... }
```

## Success Criteria

The following should work:

```rumps
; Regex matching
LET email = "user@example.com"
IF email matches /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

LET phone = "555-1234"
IF phone matches /^\d{3}-\d{4}$/ {
  OUTPUT "Valid phone"
}

; Negated match
LET input = "safe text"
IF NOT (input matches /<script>/) {
  OUTPUT "No script tags"
}

; Collection operations (requires closures from Phase 2)
LET doubled = MAP (x => x * 2) [1, 2, 3]
OUTPUT doubled  ; [2, 4, 6]

LET evens = FILTER (x => x % 2 == 0) [1, 2, 3, 4]
OUTPUT evens  ; [2, 4]

LET sum = REDUCE (acc, x => acc + x) 0 [1, 2, 3, 4]
OUTPUT sum  ; 10
```
