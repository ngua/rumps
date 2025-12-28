# Phase 5

## 5.1: Extended OUTPUT Statement

This phase extends the `OUTPUT` statement with formatting and target options.

**Prerequisites**: Phase 4 (type system) complete.

**Testing**: Each feature requires integration tests (`.rumps` script + snapshot).

**IMPORTANT**: All I/O operations MUST use `tokio`. This includes `OUTPUT` to stderr and file targets. Use `tokio::io::stderr` and `tokio::fs::write` (or similar async APIs), NOT the synchronous `std::io` equivalents.

---

### 5.1. Extended OUTPUT Statement

The current `OUTPUT expr` statement writes to stdout. We extend it with:
- **Format modifiers**: `JSON` (convert to JSON before output)
- **Target modifiers**: `TO ERROR` (stderr), `TO FILE "path"` (file)
- **Combinations**: `OUTPUT x JSON TO FILE "path"`

#### 5.1.1 Syntax

```rumps
; Current (unchanged)
OUTPUT x

; Format: convert to JSON first
OUTPUT x JSON

; Target: output to stderr
OUTPUT x TO ERROR

; Target: output to file
OUTPUT x TO FILE "/tmp/out.txt"
OUTPUT x TO FILE path-var        ; FilePath variable

; Combined: format + target
OUTPUT x JSON TO ERROR
OUTPUT x JSON TO FILE "/tmp/out.json"
```

**Grammar**:
```
output_stmt := OUTPUT expr [format] [target]
format      := JSON
target      := TO ERROR | TO FILE expr
```

#### 5.1.2 Contextual Identifiers (Not Keywords)

**Critical requirement**: `TO`, `JSON`, `ERROR`, `FILE` must NOT be global keywords. They should only be recognized specially in the `OUTPUT` context. This preserves backward compatibility:

```rumps
; These must continue to work:
LET json = { "a": 1 }          ; `json` as variable name
LET to = 10                     ; `to` as variable name
LET error = "something failed"  ; `error` as variable name
LET file = "/path/to/file"      ; `file` as variable name

; These are the new OUTPUT modifiers:
OUTPUT data JSON                ; `JSON` recognized after OUTPUT expr
OUTPUT data TO ERROR            ; `TO ERROR` recognized after OUTPUT expr
```

**Implementation approach**: In the parser, after consuming `OUTPUT expr`, optionally match identifiers whose value (case-insensitive) equals `"JSON"`, `"TO"`, `"ERROR"`, or `"FILE"`. Use `filter_map` on `Token::Ident(s)` to check the identifier string.

#### 5.1.3 Lexer

No changes required. `TO`, `JSON`, `ERROR`, `FILE` remain as regular identifiers (`Token::Ident`).

#### 5.1.4 Parser / CST

##### 5.1.4.1 Add Output Modifier Types

Add to `parser/cst.rs`:

```rust
/// Output format modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Default: stringify the value.
    Default,
    /// Convert to JSON before output.
    Json,
}

/// Output target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputTarget {
    /// Default: stdout.
    Stdout,
    /// Write to stderr.
    Stderr,
    /// Write to a file (path expression).
    File(Expr),
}

/// Extended output statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputStmt {
    pub(crate) expr: Expr,
    pub(crate) format: OutputFormat,
    pub(crate) target: OutputTarget,
}
```

##### 5.1.4.2 Update StmtKind

Change `StmtKind::Output(Expr)` to `StmtKind::Output(OutputStmt)`.

##### 5.1.4.3 Parser Implementation

**NOTE**: As with keywords, `OUTPUT` modifiers must be case-insensitive. E.g. `output x json to error`

Update `output_stmt` in `parser.rs`:

```rust
/// Parse contextual identifier (case-insensitive match).
fn ctx_ident(expected: &'static str) -> impl Parser<Token, (), Error = ParseErr> + Clone {
    filter_map(move |span, tok| match &tok {
        Token::Ident(s) if s.eq_ignore_ascii_case(expected) => Ok(()),
        _ => Err(Simple::expected_input_found(
            span,
            Some(Some(Token::Ident(expected.to_string()))),
            Some(tok),
        )),
    })
}

/// `OUTPUT expr [JSON] [TO ERROR | TO FILE expr]`
fn output_stmt(
    stmt: impl Parser<Token, cst::Stmt, Error = ParseErr> + Clone + 'static,
) -> impl Parser<Token, cst::Stmt, Error = ParseErr> {
    let format = ctx_ident("JSON")
        .to(cst::OutputFormat::Json)
        .or_not()
        .map(|f| f.unwrap_or(cst::OutputFormat::Default));

    let to_error = ctx_ident("TO")
        .ignore_then(ctx_ident("ERROR"))
        .to(cst::OutputTarget::Stderr);

    let to_file = ctx_ident("TO")
        .ignore_then(ctx_ident("FILE"))
        .ignore_then(Self::expr(stmt.clone()))
        .map(cst::OutputTarget::File);

    let target = to_error
        .or(to_file)
        .or_not()
        .map(|t| t.unwrap_or(cst::OutputTarget::Stdout));

    just(Token::Output)
        .ignore_then(Self::expr(stmt))
        .then(format)
        .then(target)
        .map_with_span(|((expr, format), target), span| {
            let output = cst::OutputStmt { expr, format, target };
            cst::Stmt::new(cst::StmtKind::Output(output), span)
        })
}
```

**Parsing order matters**: The format (`JSON`) must come before the target (`TO ...`) to avoid ambiguity. `OUTPUT x TO ERROR` should not try to parse `TO` as a format.

#### 5.1.5 AST

##### 5.1.5.1 Add AST Types

Add to `ast.rs`:

```rust
/// Output format modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    /// Default: stringify the value.
    Default,
    /// Convert to JSON before output.
    Json,
}

/// Output target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutputTarget {
    /// Default: stdout.
    Stdout,
    /// Write to stderr.
    Stderr,
    /// Write to a file (path expression).
    File(ExprId),
}

/// Extended output statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputStmt {
    pub(crate) expr: ExprId,
    pub(crate) format: OutputFormat,
    pub(crate) target: OutputTarget,
}
```

##### 5.1.5.2 Update Stmt Enum

Change `Stmt::Output(ExprId)` to `Stmt::Output(OutputStmt)`.

#### 5.1.6 Lowering (CST -> AST)

Update `parser/lower.rs` to convert `cst::OutputStmt` to `ast::OutputStmt`:

```rust
cst::StmtKind::Output(output) => {
    let expr_id = lower_expr(ast, output.expr)?;
    let format = match output.format {
        cst::OutputFormat::Default => ast::OutputFormat::Default,
        cst::OutputFormat::Json => ast::OutputFormat::Json,
    };
    let target = match output.target {
        cst::OutputTarget::Stdout => ast::OutputTarget::Stdout,
        cst::OutputTarget::Stderr => ast::OutputTarget::Stderr,
        cst::OutputTarget::File(path_expr) => {
            let path_id = lower_expr(ast, path_expr)?;
            ast::OutputTarget::File(path_id)
        }
    };
    Stmt::Output(ast::OutputStmt { expr: expr_id, format, target })
}
```

#### 5.1.7 Typechecker

Update `typecheck/infer/stmt.rs`:

```rust
fn output(&mut self, output: &OutputStmt, span: Span) {
    let expr_ty = self.expr(output.expr);

    // Format constraint
    match output.format {
        OutputFormat::Default => {
            // All types are stringable
            self.constrain(Constraint::Stringable(expr_ty, span));
        }
        OutputFormat::Json => {
            // Must be JSON-convertible (not closures, etc.)
            self.constrain(Constraint::Jsonable(expr_ty, span));
        }
    }

    // Target constraint
    match &output.target {
        OutputTarget::Stdout | OutputTarget::Stderr => {}
        OutputTarget::File(path_expr) => {
            // Path must be FilePath or String (coercible to FilePath)
            let path_ty = self.expr(*path_expr);
            let file_path_ty = Ty::Named(TypeId::FILE_PATH, vec![]);
            let string_ty = Ty::String;
            // Either FilePath or String is acceptable
            let union_ty = Ty::Union(vec![file_path_ty, string_ty]);
            self.unify(path_ty, union_ty, span);
        }
    }
}
```

#### 5.1.8 Interpreter

Update `interpreter.rs`:

```rust
async fn output(&mut self, output: &OutputStmt) -> Result<()> {
    let span = self.ast.expr_span(output.expr).unwrap_or_default();
    let val = self.eval(output.expr).await?;

    // Apply format
    let text = match output.format {
        OutputFormat::Default => self.display(&val),
        OutputFormat::Json => {
            let json = self.jsonify(&val, span)?;
            serde_json::to_string_pretty(&json)
                .map_err(|e| Error::runtime(format!("JSON serialization failed: {e}"), span))?
        }
    };

    // Write to target
    match &output.target {
        OutputTarget::Stdout => self.io.stdout(&text, span).await,
        OutputTarget::Stderr => self.io.stderr(&text, span).await,
        OutputTarget::File(path_expr) => {
            let path_val = self.eval(*path_expr).await?;
            let path = self.to_file_path(&path_val, span)?;
            self.io.write_file(&path, &text, span).await
        }
    }
}
```

##### 5.1.8.1 IoContext Extensions

The `IoContext` trait needs `stderr` and `write_file` methods.

**NOTE**: All implementations MUST use `tokio` async I/O:
- `stderr`: use `tokio::io::stderr()` with `AsyncWriteExt`
- `write_file`: use `tokio::fs::write()` or `tokio::fs::File`

```rust
#[async_trait]
pub trait IoContext {
    // Existing
    async fn stdout(&mut self, s: &str, span: Span) -> Result<()>;

    // New
    async fn stderr(&mut self, s: &str, span: Span) -> Result<()>;
    async fn write(&mut self, path: &Path, content: &str, span: Span) -> Result<()>;
}
```

#### 5.1.9 Tests

- [ ] Parser tests for all syntax variants
- [ ] Typechecker tests for format/target constraints
- [ ] Interpreter tests for stdout/stderr/file output
- [ ] Integration test script (`101_output_extended.rumps`)

#### 5.1.10 Implementation Checklist

- [ ] **CST**: Add `OutputFormat`, `OutputTarget`, `OutputStmt` types
- [ ] **CST**: Update `StmtKind::Output` to use `OutputStmt`
- [ ] **Parser**: Implement contextual identifier matching (`ctx_ident`)
- [ ] **Parser**: Update `output_stmt` to parse format and target
- [ ] **AST**: Add `OutputFormat`, `OutputTarget`, `OutputStmt` types
- [ ] **AST**: Update `Stmt::Output` to use `OutputStmt`
- [ ] **Lowering**: Update CST->AST conversion for `Output`
- [ ] **Typechecker**: Add format (`Jsonable`) and target (`FilePath | String`) constraints
- [ ] **Interpreter**: Implement format application (stringify vs JSON)
- [ ] **Interpreter**: Implement target routing (stdout/stderr/file)
- [ ] **IoContext**: Add `stderr` method
- [ ] **IoContext**: Add `write_file` method (or reuse `Io.Directory.write-file` logic)
- [ ] **Tests**: Parser unit tests
- [ ] **Tests**: Typechecker unit tests
- [ ] **Tests**: Interpreter unit tests
- [ ] **Tests**: Integration test script

---

### Design Notes

#### Why Contextual Identifiers?

Adding `TO`, `JSON`, `ERROR`, `FILE` as global keywords would:
1. Break existing code using these as variable names
2. Pollute the keyword namespace unnecessarily
3. Make the language less predictable

By parsing them contextually (only after `OUTPUT expr`), we:
1. Keep the keyword set minimal
2. Allow natural variable naming

#### Parser Approach

The parser uses a lookahead pattern:
1. Parse `OUTPUT expr` as before
2. Optionally match `Ident("JSON")` for format
3. Optionally match `Ident("TO")` followed by target

This is straightforward with chumsky's combinators and `filter_map`.

#### Alternative: Special Tokens

An alternative would be to add `Token::Json`, `Token::To`, etc., and make them "soft keywords" that the lexer emits only in certain states. This is more complex and not necessary for our use case.
